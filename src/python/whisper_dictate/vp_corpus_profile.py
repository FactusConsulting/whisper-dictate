"""Pure corpus filter-profiles: select a subset of corpus items by language/category.

Feature (B) of the 1.13 wave. The golden corpus (``benchmark/corpus.json``) tags
every item with a ``language`` (``da`` / ``en``) and a fine-grained ``category``
(``mixed_technical``, ``product_names``, ``short_danish`` …). Both the benchmark
and the corpus->dictionary training (Feature A) want to target a *subset* — e.g.
"only the Danish technical items" — without the caller memorising the exact
category strings.

This module is PURE (no IO, no model load): it operates on already-parsed
``CorpusItem`` objects (the dataclass from :mod:`vp_benchmark`) and never touches
audio. Two layers:

* a small named-group vocabulary (:data:`CATEGORY_GROUPS`) mapping friendly group
  names (``technical``, ``business``, ``names`` …) onto the concrete categories
  present in the shipped corpus, so ``--category technical`` works without the
  user knowing it expands to ``mixed_technical``/``english_technical``/``terminal``;
* :func:`filter_corpus_items`, which applies an optional language filter AND an
  optional category/group filter (case-insensitive, multi-value), defaulting to
  "all items" so the existing benchmark behaviour is unchanged when no profile is
  given.

Keeping it egui-agnostic (no UI hooks) is deliberate — the Rust UI selector is
deferred until the egui 0.34 upgrade lands on its own branch.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable, Protocol, Sequence


class _HasLanguageAndCategory(Protocol):
    """Structural type for the bits of a corpus item this module reads.

    Declared as a Protocol so the pure filter works against the real
    :class:`vp_benchmark.CorpusItem` AND lightweight test doubles without an
    import cycle back into the benchmark module.
    """

    language: str
    category: str


# Friendly category-group names -> the concrete ``category`` strings that appear
# in the shipped ``benchmark/corpus.json``. A group expands to its members so a
# profile like ``technical`` selects every technical-flavoured category without
# the caller naming each one. Group names and members are matched
# case-insensitively (see :func:`_expand_categories`). Keep this in sync with the
# corpus categories; unknown categories simply never match a group (and can still
# be selected by their exact name).
CATEGORY_GROUPS: dict[str, tuple[str, ...]] = {
    # Technical / engineering-flavoured items (commands, infra, code talk).
    "technical": ("mixed_technical", "english_technical", "terminal"),
    # Product / brand / people names that must keep exact spelling.
    "business": ("product_names", "names"),
    "names": ("product_names", "names"),
    "products": ("product_names",),
    # Short utterances in either language.
    "short": ("short_danish", "short_english"),
    # Long-form paragraphs.
    "long": ("long_danish",),
    # UI / workflow phrasing.
    "ui": ("ui_workflow",),
    "workflow": ("ui_workflow",),
    # Robustness buckets.
    "noise": ("noise_sensitive",),
    "punctuation": ("punctuation",),
    "layout": ("layout",),
    "mixed": ("mixed_language", "mixed_technical"),
}


@dataclass(frozen=True)
class CorpusProfile:
    """A normalised selection over the corpus: optional languages + categories.

    Both filters are tuples of casefolded tokens. Empty means "no constraint on
    this axis" so an empty profile (the default) selects every item. ``categories``
    holds concrete corpus category strings AFTER group expansion, so matching is a
    plain membership test.
    """

    languages: tuple[str, ...] = ()
    categories: tuple[str, ...] = ()

    @property
    def is_empty(self) -> bool:
        """True when neither axis constrains anything (selects all items)."""
        return not self.languages and not self.categories

    def describe(self) -> str:
        """Human-readable one-liner for CLI preview/help (never raises)."""
        if self.is_empty:
            return "all items"
        parts: list[str] = []
        if self.languages:
            parts.append(f"language={'/'.join(self.languages)}")
        if self.categories:
            parts.append(f"category={'/'.join(sorted(self.categories))}")
        return ", ".join(parts)


def _split_tokens(value: str | Iterable[str] | None) -> list[str]:
    """Normalise a raw selector into a list of casefolded, non-empty tokens.

    Accepts ``None``, a comma-separated string (``"da,en"``) or any iterable of
    strings, so the CLI can pass ``--language da,en`` or a list interchangeably.
    Whitespace is trimmed and empties dropped; order is preserved minus dupes.
    """
    if value is None:
        return []
    if isinstance(value, str):
        raw = value.split(",")
    else:
        raw = []
        for part in value:
            raw.extend(str(part).split(","))
    seen: set[str] = set()
    out: list[str] = []
    for token in raw:
        norm = token.strip().casefold()
        if norm and norm not in seen:
            seen.add(norm)
            out.append(norm)
    return out


def _expand_categories(tokens: Sequence[str]) -> tuple[str, ...]:
    """Expand category/group tokens into concrete corpus category strings.

    Each token is looked up in :data:`CATEGORY_GROUPS` (case-insensitively); a
    group expands to its members, while a non-group token is kept verbatim so an
    exact category name (``mixed_technical``) still works. Results are casefolded
    and de-duplicated preserving first-seen order.
    """
    seen: set[str] = set()
    out: list[str] = []
    for token in tokens:
        members = CATEGORY_GROUPS.get(token)
        expanded = members if members is not None else (token,)
        for category in expanded:
            norm = category.strip().casefold()
            if norm and norm not in seen:
                seen.add(norm)
                out.append(norm)
    return tuple(out)


def build_profile(
    *,
    language: str | Iterable[str] | None = None,
    category: str | Iterable[str] | None = None,
) -> CorpusProfile:
    """Build a normalised :class:`CorpusProfile` from raw CLI selectors.

    ``language`` and ``category`` each accept a comma-separated string or an
    iterable; categories are group-expanded via :data:`CATEGORY_GROUPS`. An
    all-empty/blank input yields an empty profile (selects everything), so the
    default benchmark/training behaviour is preserved.
    """
    languages = tuple(_split_tokens(language))
    categories = _expand_categories(_split_tokens(category))
    return CorpusProfile(languages=languages, categories=categories)


def matches_profile(item: _HasLanguageAndCategory, profile: CorpusProfile) -> bool:
    """Whether a single corpus item satisfies the profile (pure predicate).

    Empty axes never exclude. Language and category are matched case-insensitively
    against the item's own ``language`` / ``category``. Both axes are ANDed: an
    item must match the language constraint AND the category constraint when each
    is present.
    """
    if profile.is_empty:
        return True
    if profile.languages:
        if (item.language or "").strip().casefold() not in profile.languages:
            return False
    if profile.categories:
        if (item.category or "").strip().casefold() not in profile.categories:
            return False
    return True


def filter_corpus_items(
    items: Iterable[_HasLanguageAndCategory],
    profile: CorpusProfile,
) -> list[_HasLanguageAndCategory]:
    """Return the subset of ``items`` matching ``profile`` (order preserved).

    The single entry point used by the benchmark and the training features. With
    an empty profile this returns every item unchanged, so callers can always run
    it without special-casing "no filter".
    """
    return [item for item in items if matches_profile(item, profile)]


def known_languages(items: Iterable[_HasLanguageAndCategory]) -> list[str]:
    """Sorted, de-duplicated list of languages present in ``items`` (for help)."""
    seen = {(item.language or "").strip().casefold() for item in items}
    return sorted(lang for lang in seen if lang)


def known_categories(items: Iterable[_HasLanguageAndCategory]) -> list[str]:
    """Sorted, de-duplicated list of categories present in ``items`` (for help)."""
    seen = {(item.category or "").strip().casefold() for item in items}
    return sorted(category for category in seen if category)
