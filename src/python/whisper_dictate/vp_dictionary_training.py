"""Pure logic for corpus->dictionary training (Feature A of the 1.13 wave).

Two ways to grow the Whisper-prompt dictionary from the golden corpus, both pure
(no IO, no model load, no audio) so they unit-test in isolation. The IO (reading
the corpus / the dictionary file, writing it back) lives in
:mod:`vp_dictionary_store` and :mod:`vp_dictionary_training_cli`.

CLARIFICATION (this was a past confusion): the *corpus* is the benchmark golden
set — pre-recorded audio plus reference TEXT. These features read the corpus
*text* (and benchmark result rows) to propose dictionary terms. They NEVER record
or touch audio; recording corpus audio is the separate ``--record-corpus-item``
mode.

build-from-corpus
    :func:`extract_candidate_terms` mines domain-ish terms from corpus reference
    texts — the explicit per-item ``terms`` (curated gold), plus capitalised /
    multi-word / technical-looking tokens, frequency-filtered to drop one-off
    noise. :func:`merge_terms` then APPENDS them to the existing dictionary,
    de-duping case-insensitively, returning a preview of what *would* be added.

suggest-from-benchmark-misses
    :func:`suggest_terms_from_misses` reads benchmark result rows (the per-item
    reference vs hypothesis with ``term_misses``) and surfaces the domain terms
    the model got wrong as SUGGESTED additions — never auto-applied; the CLI
    previews them and the user/automation confirms.
"""
from __future__ import annotations

import re
from collections import Counter
from dataclasses import dataclass, field
from typing import Any, Iterable

# Tokeniser: words may contain Danish letters, internal hyphens and dots
# (so "large-v3", "signal-to-noise" and "git.tag" survive as one token). Mirrors
# the spirit of the tokenisers already in vp_benchmark / vp_dictionary_suggest.
_WORD_RE = re.compile(r"[\wæøåÆØÅ]+(?:[.\-][\wæøåÆØÅ]+)*", flags=re.UNICODE)

# Sentence-initial / generic words that are capitalised only because they start a
# sentence — not domain terms. Lower-cased; both Danish and English fillers. A
# capitalised single token whose lower-case form is here is NOT a candidate.
_STOPWORDS = {
    # English
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "create",
    "do", "for", "from", "i", "if", "in", "is", "it", "its", "me", "new", "of",
    "on", "or", "please", "run", "set", "should", "tell", "the", "their", "them",
    "then", "they", "this", "to", "via", "want", "whether", "with", "you",
    # Danish (entries shared with English above, e.g. "at"/"for", are intentionally
    # not repeated here — the set is the union; repeating them is a silent no-op).
    "af", "behold", "bliver", "brug", "commit", "der", "det", "du", "eller",
    "en", "er", "et", "fra", "gerne", "her", "hvis", "hvor", "ikke", "jeg",
    "kan", "lad", "lave", "lige", "med", "modellen", "nye", "og", "om", "op",
    "opret", "os", "på", "se", "skal", "skift", "som", "stadig", "så", "til",
    "tjek", "vi", "vil", "være",
}


@dataclass(frozen=True)
class TermCandidate:
    """A proposed dictionary term plus why it was proposed (for the preview).

    ``count`` is how many corpus ITEMS mentioned it (each item contributes at most
    once, regardless of how many times — or via how many heuristics — it appears in
    that item). ``reason`` is one of ``curated_term`` (came from a corpus item's
    ``terms``), ``multi_word``, ``capitalized`` or ``technical``. ``samples`` keeps a
    few source ids/snippets.
    """

    term: str
    count: int = 1
    reason: str = ""
    samples: tuple[str, ...] = ()


@dataclass
class MergePreview:
    """Outcome of merging candidates into the existing dictionary (no IO done).

    ``added`` is the new terms (in candidate order) that would be appended;
    ``skipped_existing`` already present (case-insensitively); ``result_terms`` is
    the full term list after the merge. ``existing_count`` is the pre-merge size.
    This is what the CLI prints in ``--dry-run``/preview mode before any write.
    """

    added: list[str] = field(default_factory=list)
    skipped_existing: list[str] = field(default_factory=list)
    result_terms: list[str] = field(default_factory=list)
    existing_count: int = 0

    @property
    def added_count(self) -> int:
        return len(self.added)


@dataclass(frozen=True)
class TermSuggestion:
    """A SUGGESTED dictionary term from benchmark misses (preview, confirm first).

    ``term`` is the missed domain term; ``count`` how many result rows missed it;
    ``samples`` a few corpus ids. ``already_in_dictionary`` flags that the term is
    present already (so the CLI can de-emphasise it / the merge can skip it).
    """

    term: str
    count: int = 1
    samples: tuple[str, ...] = ()
    already_in_dictionary: bool = False


def _normalize(term: str) -> str:
    """Case-insensitive comparison key for a term (casefold + collapse spaces)."""
    return " ".join((term or "").split()).casefold()


def _words(text: str) -> list[str]:
    return _WORD_RE.findall(text or "")


def _is_capitalized_token(token: str) -> bool:
    """A single token that looks like a name: starts upper, not ALL-lower, not a stopword.

    Accepts ``Kubernetes``, ``OpenClaw``, ``GitHub`` (internal caps) but rejects
    sentence-initial fillers (``The``, ``Skift``) via the stopword set, lone capital
    letters (``X``, ``I``) and tokens with no alphabetic character at all.
    """
    if not token or not token[0].isupper():
        return False
    if _normalize(token) in _STOPWORDS:
        return False
    # Reject a bare single capital letter (e.g. "X", "A") — a lone letter is noise,
    # not a domain term, even when it isn't in the stopword set.
    if len(token) == 1:
        return False
    # Must contain at least one letter (drops digit-/symbol-only tokens like "3.").
    return any(ch.isalpha() for ch in token)


def _is_technical_token(token: str) -> bool:
    """A token that looks technical: ALL-CAPS acronym, has digits, or hyph/dot-joined.

    Captures ``MCP``, ``RAG``, ``vLLM``, ``large-v3``, ``signal-to-noise`` and
    ``RTX`` while excluding plain lowercase words. Short pure-lowercase words are
    NOT technical (they fall to the frequency filter via curated/multi-word paths).
    """
    if not token:
        return False
    letters = [ch for ch in token if ch.isalpha()]
    if letters and all(ch.isupper() for ch in letters) and len(token) >= 2:
        return True  # acronym, e.g. MCP, RAG, STT, RTX
    if any(ch.isdigit() for ch in token) and any(ch.isalpha() for ch in token):
        return True  # mixed alnum, e.g. large-v3, gpt-4o
    if ("-" in token or "." in token) and len(token) >= 4:
        return True  # hyphen/dot-joined compound, e.g. signal-to-noise
    # Internal/non-leading uppercase signals a studly/camel-case product name even
    # when the first letter is lower-case, e.g. vLLM, iPhone, OpenAI.
    if len(token) >= 3 and any(ch.isupper() for ch in token[1:]):
        return True
    return False


def _curated_terms(items_terms: Iterable[Iterable[str]]) -> Counter:
    """Count the curated per-item ``terms`` across the (already filtered) corpus."""
    counter: Counter = Counter()
    for terms in items_terms:
        for term in terms or []:
            cleaned = (term or "").strip()
            if cleaned:
                counter[cleaned] += 1
    return counter


# Punctuation that breaks a capitalised run, so "OpenClaw, MCP, RAG" yields three
# single tokens rather than one bogus "OpenClaw MCP RAG" phrase. Splitting the text
# on these first means only ADJACENT capitalised words (real phrases like "Claude
# Code", "Windows Terminal") group together.
_SEGMENT_RE = re.compile(r"[^,;:.!?()\[\]{}\"']+")


def _multi_word_candidates(text: str) -> list[str]:
    """Capitalised multi-word runs in ``text`` (e.g. "Claude Code", "Windows Terminal").

    A run of consecutive capitalised tokens (length >= 2) within a single
    punctuation-delimited segment is a strong proper-noun signal. The text is split
    on commas/semicolons/sentence punctuation first so a comma-separated list
    ("OpenClaw, MCP, RAG") does NOT collapse into one phrase. A leading token may
    be sentence-initial, but the per-token capitalised check (which excludes
    stopwords) keeps that from producing junk.
    """
    runs: list[str] = []
    for segment in _SEGMENT_RE.findall(text or ""):
        current: list[str] = []
        for token in _words(segment):
            if _is_capitalized_token(token):
                current.append(token)
                continue
            if len(current) >= 2:
                runs.append(" ".join(current))
            current = []
        if len(current) >= 2:
            runs.append(" ".join(current))
    return runs


def _single_token_candidates(text: str) -> list[tuple[str, str]]:
    """Single-token candidates with their reason: (token, "capitalized"|"technical")."""
    out: list[tuple[str, str]] = []
    for token in _words(text):
        if _is_technical_token(token):
            out.append((token, "technical"))
        elif _is_capitalized_token(token):
            out.append((token, "capitalized"))
    return out


def _aligned_ids(item_ids: Iterable[str] | None, n: int) -> list[str]:
    """Per-text sample ids, padded with ``""`` so every text has a matching slot."""
    ids = list(item_ids) if item_ids is not None else [""] * n
    if len(ids) < n:
        ids = ids + [""] * (n - len(ids))
    return ids


def _item_hits(text: str, curated: Iterable[str]) -> list[tuple[str, str]]:
    """All ``(term, reason)`` hits found in one corpus item (curated first).

    Curated terms lead so they own the surface form / reason; then capitalised
    multi-word runs and the single capitalised/technical tokens. The caller
    de-duplicates per item, so repeats within one item don't inflate the count.
    """
    hits: list[tuple[str, str]] = [
        (term, "curated_term") for term in _curated_terms([curated])
    ]
    hits.extend((run, "multi_word") for run in _multi_word_candidates(text))
    hits.extend(_single_token_candidates(text))
    return hits


class _CandidateAccumulator:
    """Tallies term candidates by counting each term once per corpus ITEM.

    ``count`` therefore means "how many corpus items mentioned the term" (matching
    :class:`TermCandidate`'s contract) — repeated mentions within a single item, or
    the same term surfacing via several heuristics in one item, are counted once.
    The first non-empty surface/reason wins, except a ``curated_term`` reason
    upgrades a weaker one. Curated terms are remembered so the merge can force-keep
    them regardless of ``min_count``.
    """

    def __init__(self) -> None:
        self._counts: Counter = Counter()
        self._reasons: dict[str, str] = {}
        self._surface: dict[str, str] = {}
        self._samples: dict[str, list[str]] = {}
        self._curated: set[str] = set()

    def add_item(self, text: str, curated: Iterable[str], sample_id: str) -> None:
        """Record one corpus item's hits, counting each distinct term just once."""
        counted: set[str] = set()
        for term, reason in _item_hits(text, curated):
            norm = self._record_surface(term, reason, sample_id)
            if norm and norm not in counted:
                counted.add(norm)
                self._counts[norm] += 1

    def _record_surface(self, term: str, reason: str, sample_id: str) -> str:
        """Update surface/reason/samples for ``term``; return its norm key ("" if blank)."""
        cleaned = (term or "").strip()
        norm = _normalize(cleaned)
        if not norm:
            return ""
        first_seen = norm not in self._surface
        curated_upgrade = reason == "curated_term" and self._reasons.get(norm) != "curated_term"
        if first_seen or curated_upgrade:
            self._surface[norm] = cleaned
            self._reasons[norm] = reason
        if reason == "curated_term":
            self._curated.add(norm)
        if sample_id and sample_id not in self._samples.setdefault(norm, []):
            self._samples[norm].append(sample_id)
        return norm

    def results(self, min_count: int) -> list[TermCandidate]:
        """Build the sorted candidate list, force-keeping curated terms."""
        threshold = max(1, min_count)
        candidates = [
            TermCandidate(
                term=self._surface[norm],
                count=count,
                reason=self._reasons.get(norm, ""),
                samples=tuple(self._samples.get(norm, [])[:5]),
            )
            for norm, count in self._counts.items()
            if norm in self._curated or count >= threshold
        ]
        candidates.sort(key=lambda c: (-c.count, c.term.casefold()))
        return candidates


def extract_candidate_terms(
    texts: Iterable[str],
    *,
    item_terms: Iterable[Iterable[str]] | None = None,
    item_ids: Iterable[str] | None = None,
    min_count: int = 1,
) -> list[TermCandidate]:
    """Mine domain-term candidates from corpus reference texts (PURE).

    Inputs are the per-item reference ``texts``, optionally the curated per-item
    ``terms`` (counted as high-confidence ``curated_term`` candidates) and matching
    ``item_ids`` (used as preview samples). Candidates are gathered from:

    * curated per-item terms (always kept regardless of ``min_count``);
    * capitalised multi-word runs ("Claude Code");
    * single capitalised tokens ("Kubernetes") and technical tokens ("MCP",
      "large-v3"), each frequency-filtered by ``min_count`` so one-off noise from a
      single sentence is dropped when ``min_count > 1``.

    Each term is counted at most ONCE per corpus item (so ``count`` is the number of
    items that mention it, not raw occurrences). De-duplicated case-insensitively,
    preferring the curated/first-seen surface form. Returns :class:`TermCandidate`
    objects sorted by descending count then case-insensitive term, so the
    most-mentioned domain terms preview first.
    """
    texts = list(texts)
    curated_per_item = list(item_terms) if item_terms is not None else []
    ids = _aligned_ids(item_ids, len(texts))

    acc = _CandidateAccumulator()
    for index, (text, sample_id) in enumerate(zip(texts, ids)):
        curated = curated_per_item[index] if index < len(curated_per_item) else ()
        acc.add_item(text, curated, sample_id)
    return acc.results(min_count)


def merge_terms(existing: Iterable[str], candidates: Iterable[Any]) -> MergePreview:
    """Append candidate terms to ``existing``, de-duping case-insensitively (PURE).

    ``candidates`` may be plain strings or :class:`TermCandidate` objects (their
    ``.term`` is used). Existing terms are preserved in order; each new candidate
    is appended only if its normalised form is not already present (in ``existing``
    or among earlier-added candidates). Returns a :class:`MergePreview` describing
    what would be added/skipped and the resulting term list — NO file is written
    here (that is the CLI's job, gated behind preview/confirmation).
    """
    existing_terms = [str(t).strip() for t in existing if str(t).strip()]
    seen = {_normalize(t) for t in existing_terms}
    skipped_seen: set[str] = set()
    preview = MergePreview(
        result_terms=list(existing_terms),
        existing_count=len(existing_terms),
    )
    for candidate in candidates:
        term = candidate.term if isinstance(candidate, TermCandidate) else str(candidate)
        term = term.strip()
        norm = _normalize(term)
        if not norm:
            continue
        if norm in seen:
            # Dedup the skipped list by NORMALIZED form, so two differently-cased
            # candidates for the same existing term ("Kubectl"/"kubectl") don't
            # both land in skipped_existing.
            if norm not in skipped_seen:
                skipped_seen.add(norm)
                preview.skipped_existing.append(term)
            continue
        seen.add(norm)
        preview.added.append(term)
        preview.result_terms.append(term)
    return preview


def _row_corpus_id(row: dict[str, Any]) -> str:
    return str(row.get("corpus_id") or row.get("id") or row.get("source_file") or "")


def _row_term_misses(row: dict[str, Any]) -> list[str]:
    """Domain terms a benchmark row missed (the annotated ``term_misses`` field)."""
    misses = row.get("term_misses")
    if isinstance(misses, str):
        misses = [misses]
    return [str(t).strip() for t in (misses or []) if str(t).strip()]


def suggest_terms_from_misses(
    rows: Iterable[dict[str, Any]],
    *,
    existing_terms: Iterable[str] = (),
    min_count: int = 1,
) -> list[TermSuggestion]:
    """Surface domain terms the benchmark got wrong as SUGGESTED additions (PURE).

    Reads annotated benchmark result rows (the JSONL the benchmark emits with
    ``term_misses`` per item). Each missed term is a suggestion to add to the
    dictionary so the prompt biases the model toward it next run. Suggestions are
    counted across rows, frequency-filtered by ``min_count``, flagged when already
    present in ``existing_terms``, and sorted by descending count then term. These
    are NEVER auto-applied — the CLI previews them and the caller confirms.
    """
    known = {_normalize(t) for t in existing_terms if _normalize(t)}
    counts: Counter = Counter()
    surface: dict[str, str] = {}
    samples: dict[str, list[str]] = {}

    for row in rows:
        sample = _row_corpus_id(row)
        for term in _row_term_misses(row):
            norm = _normalize(term)
            if not norm:
                continue
            counts[norm] += 1
            surface.setdefault(norm, term)
            if sample and sample not in samples.setdefault(norm, []):
                samples[norm].append(sample)

    suggestions: list[TermSuggestion] = []
    for norm, count in counts.items():
        if count < max(1, min_count):
            continue
        suggestions.append(TermSuggestion(
            term=surface[norm],
            count=count,
            samples=tuple(samples.get(norm, [])[:5]),
            already_in_dictionary=norm in known,
        ))
    suggestions.sort(key=lambda s: (-s.count, s.term.casefold()))
    return suggestions
