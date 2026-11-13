// 简繁 conversion for display only, from the OpenCC dictionaries (Apache-2.0) in assets/opencc/.
// OpenCC s2t / t2s style: maximal forward matching over the phrase dictionary, then the character
// dictionary, taking the first candidate of each entry; anything not in either is kept as is.
// Pure module (no DOM): the app and the unit tests import it.

// "key<TAB>value value…" lines; '#' comments and blank lines skipped. First candidate only.
export function parseDict(text) {
  const map = new Map();
  for (const line of text.split('\n')) {
    if (!line || line[0] === '#') continue;
    const tab = line.indexOf('\t');
    if (tab <= 0) continue;
    const key = line.slice(0, tab);
    const value = line.slice(tab + 1).trim().split(' ')[0];
    if (value && !map.has(key)) map.set(key, value);
  }
  return map;
}

// convert(text): phrases (longest key first, at least 2 code points), then single characters.
export function makeConverter(phrasesText, charsText) {
  const phrases = parseDict(phrasesText); const chars = parseDict(charsText);
  // First code point → the phrase lengths (in code points) that start with it, longest first.
  const lens = new Map();
  for (const k of phrases.keys()) {
    const cps = [...k];
    if (cps.length < 2) { if (!chars.has(k)) chars.set(k, phrases.get(k)); continue; }
    const set = lens.get(cps[0]) || new Set(); set.add(cps.length); lens.set(cps[0], set);
  }
  const order = new Map([...lens].map(([c, s]) => [c, [...s].sort((a, b) => b - a)]));
  return function convert(text) {
    if (!text) return text;
    const cps = [...text]; let out = '';
    for (let i = 0; i < cps.length;) {
      const ls = order.get(cps[i]); let hit = false;
      if (ls) {
        for (const n of ls) {
          if (i + n > cps.length) continue;
          const v = phrases.get(cps.slice(i, i + n).join(''));
          if (v !== undefined) { out += v; i += n; hit = true; break; }
        }
      }
      if (hit) continue;
      const c = chars.get(cps[i]);
      out += c === undefined ? cps[i] : c; i += 1;
    }
    return out;
  };
}

// The dictionary files of each direction (fetched only on first use).
export const ZH_DICTS = {
  s: ['assets/opencc/TSPhrases.txt', 'assets/opencc/TSCharacters.txt'], // → 简体
  t: ['assets/opencc/STPhrases.txt', 'assets/opencc/STCharacters.txt'], // → 繁体
};
