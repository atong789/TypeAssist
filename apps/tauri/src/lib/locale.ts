// Practice word-bank localization.
//
// The curated bank (sentences.txt) is stored in one form; we swap the handful of
// words whose spelling differs by locale so Practice matches the user's macOS
// language/region — or an explicit override they set in Settings.
//
// Privacy (Principle #8): reading the locale is a local, in-memory read, and the
// override is a stored *preference* (not user activity / recovery data). Both
// stay on-device — nothing leaves.

export type SpellingPref = "system" | "en-US" | "en-GB" | "en-IN";
export type SpellingVariant = "american" | "british";

const PREF_KEY = "typeassist.practice.spelling";

/** The stored override, or `"system"` (follow the OS) if unset/unavailable. */
export function loadSpellingPref(): SpellingPref {
  try {
    const v = localStorage.getItem(PREF_KEY);
    if (v === "system" || v === "en-US" || v === "en-GB" || v === "en-IN") {
      return v;
    }
  } catch {
    // localStorage unavailable — fall through to the default.
  }
  return "system";
}

export function saveSpellingPref(pref: SpellingPref): void {
  try {
    localStorage.setItem(PREF_KEY, pref);
  } catch {
    // Non-fatal: the choice just won't persist this session.
  }
}

// Regions that use American spelling. Everything else English (GB, IN, AU, NZ,
// IE, ZA, CA…) keeps British spelling — the bank's existing form — so following
// the system only *fixes* American locales rather than changing everyone.
const AMERICAN_REGIONS = new Set(["US", "PH"]);

/** Region subtag from a BCP-47 tag: `"en-US"` → `"US"`, `"en"` → `""`. */
function regionOf(tag: string): string {
  return (tag.split("-")[1] ?? "").toUpperCase();
}

/** Resolve the preference to a concrete spelling variant. `"system"` reads the
 *  OS locale via `navigator.language`. */
export function resolveVariant(pref: SpellingPref): SpellingVariant {
  if (pref === "en-US") return "american";
  if (pref === "en-GB" || pref === "en-IN") return "british";
  const tag =
    (typeof navigator !== "undefined" && navigator.language) || "en-US";
  return AMERICAN_REGIONS.has(regionOf(tag)) ? "american" : "british";
}

// [american, british] for the words whose spelling diverges. The bank is small
// and curated, so this only needs the divergent words it actually uses (today:
// gray/grey); the rest are common pairs included so the bank can grow without
// another code change. `meter/metre` is intentionally omitted (the American
// "meter" is also a measuring device — ambiguous to swap blindly).
const PAIRS: [string, string][] = [
  ["gray", "grey"],
  ["harbor", "harbour"],
  ["harbors", "harbours"],
  ["color", "colour"],
  ["colors", "colours"],
  ["colored", "coloured"],
  ["favorite", "favourite"],
  ["favorites", "favourites"],
  ["favor", "favour"],
  ["behavior", "behaviour"],
  ["neighbor", "neighbour"],
  ["neighbors", "neighbours"],
  ["flavor", "flavour"],
  ["flavors", "flavours"],
  ["honor", "honour"],
  ["center", "centre"],
  ["centers", "centres"],
  ["centered", "centred"],
  ["theater", "theatre"],
  ["liter", "litre"],
  ["liters", "litres"],
  ["traveling", "travelling"],
  ["traveled", "travelled"],
  ["traveler", "traveller"],
  ["canceled", "cancelled"],
  ["realize", "realise"],
  ["realized", "realised"],
  ["realizes", "realises"],
  ["organize", "organise"],
  ["organized", "organised"],
  ["recognize", "recognise"],
  ["recognized", "recognised"],
  ["apologize", "apologise"],
  ["defense", "defence"],
  ["analyze", "analyse"],
  ["analyzed", "analysed"],
];

const TO_AMERICAN = new Map(PAIRS.map(([a, b]) => [b, a]));
const TO_BRITISH = new Map(PAIRS.map(([a, b]) => [a, b]));

/** Swap locale-divergent words to the target variant. The bank is all-lowercase
 *  with no punctuation (enforced in `sentenceBank`), so a plain space split is
 *  exact; a word not in the map is left untouched. */
export function localizeSentence(
  sentence: string,
  variant: SpellingVariant,
): string {
  const map = variant === "american" ? TO_AMERICAN : TO_BRITISH;
  return sentence
    .split(" ")
    .map((w) => map.get(w) ?? w)
    .join(" ");
}

// ---- Dev guard: the bank can't silently regress -------------------------------
//
// The bank (`sentences.txt`) is stored in British form and americanized word by
// word via `PAIRS`. A British word added to the bank but missing from `PAIRS`
// would leak through unchanged on a US locale — exactly the `harbour` bug. We
// can't carry a full US/UK dictionary, so we keep a curated reference of British
// spellings we know about; the guard (run in dev from `sentenceBank`) fails if a
// bank word is in this reference but has no `PAIRS` entry. Grow this list when a
// new divergent word joins the bank — that's the signal to add its pair too.
const BRITISH_REFERENCE: ReadonlySet<string> = new Set([
  // -our
  "harbour", "harbours", "colour", "colours", "coloured", "favour", "favours",
  "favourite", "favourites", "behaviour", "behaviours", "neighbour", "neighbours",
  "flavour", "flavours", "honour", "honours", "labour", "odour", "rumour",
  "vapour", "vigour", "armour", "parlour", "saviour", "splendour",
  // -re
  "centre", "centres", "centred", "theatre", "theatres", "litre", "litres",
  "metre", "metres", "fibre", "fibres", "calibre", "sombre", "spectre", "lustre",
  "manoeuvre",
  // -ise / -yse
  "realise", "realised", "realises", "organise", "organised", "recognise",
  "recognised", "apologise", "apologised", "analyse", "analysed", "paralyse",
  // -ce
  "defence", "offence", "licence", "pretence",
  // doubled-l
  "travelling", "travelled", "traveller", "cancelled", "modelling", "labelled",
  "marvellous", "woollen", "jewellery", "counsellor", "signalled",
  // -ogue
  "catalogue", "dialogue", "monologue",
  // misc
  "grey", "greyer", "mould", "moulded", "smoulder", "plough", "ploughed",
  "draught", "draughts", "tyre", "tyres", "kerb", "pyjamas", "aluminium",
  "programme", "cosy", "sceptical", "skilful",
]);

/** Bank words that are British spellings we recognise but have no `PAIRS` entry,
 *  so `localizeSentence(_, "american")` would leave them British. Empty = healthy.
 *  Called by `sentenceBank` in dev to hard-fail before such a word can ship. */
export function findUnpairedBritishWords(
  sentences: readonly string[],
): string[] {
  const offenders = new Set<string>();
  for (const s of sentences) {
    for (const w of s.split(" ")) {
      if (BRITISH_REFERENCE.has(w) && !TO_AMERICAN.has(w)) offenders.add(w);
    }
  }
  return [...offenders].sort();
}
