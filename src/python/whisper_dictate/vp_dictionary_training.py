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
    # Danish
    "af", "at", "behold", "bliver", "brug", "commit", "der", "det", "du", "eller",
    "en", "er", "et", "for", "fra", "gerne", "her", "hvis", "hvor", "ikke", "jeg",
    "kan", "lad", "lave", "lige", "med", "modellen", "nye", "og", "om", "op",
    "opret", "os", "på", "se", "skal", "skift", "som", "stadig", "så", "til",
    "tjek", "vi", "vil", "være",
}


@dataclass(frozen=True)
class TermCandidate:
    """A proposed dictionary term plus why it was proposed (for the preview).

    ``count`` is how many corpus sources mentioned it; ``reason`` is one of
    ``curated_term`` (came from a corpus item's ``terms``), ``multi_word``,
    ``capitalized`` or ``technical``. ``samples`` keeps a few source ids/snippets.
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
    sentence-initial fillers (``The``, ``Skift``) via the stopword set.
    """
    if not token or not token[0].isupper():
        return False
    if _normalize(token) in _STOPWORDS:
        return False
    # Reject pure ASCII single capital letters / numbers-only.
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

    De-duplicated case-insensitively, preferring the curated/first-seen surface
    form. Returns :class:`TermCandidate` objects sorted by descending count then
    case-insensitive term, so the most-mentioned domain terms preview first.
    """
    texts = list(texts)
    ids = list(item_ids) if item_ids is not None else [""] * len(texts)
    if len(ids) < len(texts):
        ids = ids + [""] * (len(texts) - len(ids))

    counts: Counter = Counter()
    reasons: dict[str, str] = {}
    surface: dict[str, str] = {}
    samples: dict[str, list[str]] = {}

    def _record(term: str, reason: str, sample: str) -> None:
        cleaned = (term or "").strip()
        norm = _normalize(cleaned)
        if not norm:
            return
        counts[norm] += 1
        # First reason/surface wins, but curated upgrades a weaker reason.
        if norm not in surface or (reason == "curated_term" and reasons.get(norm) != "curated_term"):
            surface[norm] = cleaned
            reasons[norm] = reason
        reasons.setdefault(norm, reason)
        if sample and sample not in samples.setdefault(norm, []):
            samples[norm].append(sample)

    # Curated terms first so they own the surface form / reason.
    for terms, item_id in zip(
        (item_terms if item_terms is not None else [[] for _ in texts]),
        ids,
    ):
        for term, _c in _curated_terms([terms]).items():
            _record(term, "curated_term", item_id)

    curated_norms = set(counts)  # everything curated is force-kept below

    for text, item_id in zip(texts, ids):
        for run in _multi_word_candidates(text):
            _record(run, "multi_word", item_id)
        for token, reason in _single_token_candidates(text):
            _record(token, reason, item_id)

    candidates: list[TermCandidate] = []
    for norm, count in counts.items():
        if norm not in curated_norms and count < max(1, min_count):
            continue
        candidates.append(TermCandidate(
            term=surface[norm],
            count=count,
            reason=reasons.get(norm, ""),
            samples=tuple(samples.get(norm, [])[:5]),
        ))
    candidates.sort(key=lambda c: (-c.count, c.term.casefold()))
    return candidates


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
            if term not in preview.skipped_existing:
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
