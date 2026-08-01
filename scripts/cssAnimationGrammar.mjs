/**
 * The pure, string-in/data-out half of the CSS animation lint.
 *
 * Split out from `check-css-animations.mjs` so it can be unit-tested without a
 * filesystem: see `cssAnimationGrammar.test.mjs`.
 *
 * Every entry point takes raw stylesheet text and normalises it itself, so no
 * caller can get a wrong answer by forgetting to strip comments first.
 *
 * ## Why hand-rolled instead of postcss
 *
 * postcss 8 is present in the tree, but only as a transitive dependency of
 * vite. Importing it here without declaring it would silently couple this lint
 * to vite's dependency graph, and declaring it is not possible in the
 * environment this was written in (`npm install --package-lock-only` cannot
 * write its cache). Rather than depend on a package we do not own, the pieces
 * of grammar this lint actually needs are implemented directly and covered by
 * unit tests. If postcss ever becomes a direct dependency, `keyframeSegments`
 * and `collectAnimationShorthands` are the two functions worth replacing with
 * an AST walk.
 */

/** Shorthand tokens that can never be the animation name. Compared lowercased. */
export const NON_NAME_TOKENS = new Set([
  'infinite',
  'normal',
  'reverse',
  'alternate',
  'alternate-reverse',
  'none',
  'forwards',
  'backwards',
  'both',
  'running',
  'paused',
  'linear',
  'ease',
  'ease-in',
  'ease-out',
  'ease-in-out',
  'step-start',
  'step-end',
  'initial',
  'inherit',
  'unset',
  'revert',
]);

/**
 * Matches the `animation` shorthand, optionally vendor-prefixed. The lookbehind
 * keeps it from matching custom properties such as `--card-animation:`, which
 * are not animation declarations and must not be linted as if they were.
 */
const ANIMATION_SHORTHAND = /(?<![\w-])(?:-webkit-|-moz-|-o-)?animation\s*:\s*([^;{}]+)[;}]/gi;
const ANIMATION_NAME_LONGHAND =
  /(?<![\w-])(?:-webkit-|-moz-|-o-)?animation-name\s*:\s*([^;{}]+)[;}]/gi;
const ITERATION_COUNT_LONGHAND =
  /(?<![\w-])(?:-webkit-|-moz-|-o-)?animation-iteration-count\s*:\s*([^;{}]+)[;}]/gi;

/**
 * Comments and string literals in one pass, so that whichever opens first wins
 * (`content: "/*"` must not start a comment, and a commented-out declaration
 * must not be linted).
 *
 * Strings collapse to `""` rather than vanishing so that declaration structure
 * is preserved; their contents are never CSS.
 */
const COMMENT_OR_STRING = /\/\*[\s\S]*?\*\/|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g;

/**
 * Strip everything that looks like code but is not: comments, and the contents
 * of string literals. `content: "animation: spin 1s linear infinite"` is a
 * string, not a declaration, and linting it produced a false positive.
 */
/**
 * Known gap: the payload of an UNQUOTED `url(...)` is not neutralised, so a URL
 * literally containing `animation:` would be read as a declaration. The CSS
 * grammar forbids unescaped parens, quotes, and whitespace there, which makes
 * such a URL both invalid and absent from this codebase; neutralising it would
 * mean another hand-rolled scanner for no real defect.
 */
export const stripNonCode = (css) =>
  css.replace(COMMENT_OR_STRING, (match) => (match.startsWith('/*') ? '' : '""'));

const isTime = (token) => /^-?[\d.]+m?s$/i.test(token);
const isNumber = (token) => /^-?[\d.]+$/.test(token);
const isFunction = (token) => /^[a-z-]+\(/i.test(token);

/**
 * Remove balanced `var(...)` groups. Anything inside one is unresolved at lint
 * time, so a `steps()` sitting in a `var()` fallback must not be mistaken for
 * the declaration's real timing function.
 */
export const withoutVarGroups = (value) => {
  let out = '';
  let depth = 0;
  for (let i = 0; i < value.length; i += 1) {
    // The boundary check keeps an identifier that merely ends in "var", such
    // as `--myvar(`, from being mistaken for the start of a var() group.
    const atBoundary = i === 0 || !/[\w-]/.test(value[i - 1]);
    if (depth === 0 && atBoundary && /^var\(/i.test(value.slice(i, i + 4))) {
      depth = 1;
      i += 3;
      continue;
    }
    if (depth > 0) {
      if (value[i] === '(') depth += 1;
      else if (value[i] === ')') depth -= 1;
      continue;
    }
    out += value[i];
  }
  return out;
};

/**
 * Split on commas that are not inside parentheses, so `steps(4, jump-start)`
 * survives and `spin 1s linear, fade 2s linear` separates.
 */
export const splitTopLevel = (value) => {
  const parts = [];
  let depth = 0;
  let current = '';
  for (const ch of value) {
    if (ch === '(') depth += 1;
    if (ch === ')') depth -= 1;
    if (ch === ',' && depth === 0) {
      parts.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  parts.push(current);
  return parts.map((part) => part.trim()).filter(Boolean);
};

/** Split on whitespace that is not inside parentheses. */
export const tokenize = (value) => {
  const tokens = [];
  let depth = 0;
  let current = '';
  for (const ch of value) {
    if (ch === '(') depth += 1;
    if (ch === ')') depth -= 1;
    if (/\s/.test(ch) && depth === 0) {
      if (current) tokens.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  if (current) tokens.push(current);
  return tokens;
};

/**
 * The animation name in a shorthand: the first token that is not a keyword, a
 * time, a number, or a function. Keyword matching is case-insensitive (CSS
 * keywords are), but the name is returned with its original case because
 * `@keyframes` names are case-sensitive.
 *
 * Returns null when the name is behind a `var()`, which the caller must treat
 * as unverifiable rather than absent.
 */
export const animationNameFrom = (shorthand) => {
  for (const token of tokenize(shorthand)) {
    if (NON_NAME_TOKENS.has(token.toLowerCase())) continue;
    if (isTime(token) || isNumber(token) || isFunction(token)) continue;
    if (token.startsWith('--')) return null;
    return token;
  }
  return null;
};

/** The first time value in a shorthand, in seconds. */
export const durationSecondsFrom = (shorthand) => {
  for (const token of tokenize(shorthand)) {
    if (!isTime(token)) continue;
    return token.toLowerCase().endsWith('ms')
      ? Number.parseFloat(token) / 1000
      : Number.parseFloat(token);
  }
  return null;
};

/**
 * The step count of a `steps(n)` / `steps(n, position)` timing function.
 * A `steps()` inside a `var()` fallback does not count: it may never apply.
 */
export const stepCountFrom = (shorthand) => {
  const digits = /steps\(\s*(\d+)/i.exec(withoutVarGroups(shorthand))?.[1];
  return digits == null ? null : Number.parseInt(digits, 10);
};

export const hasInfinite = (shorthand) =>
  tokenize(shorthand).some((token) => token.toLowerCase() === 'infinite');

/** Every `animation` shorthand in the stylesheet, one entry per comma-separated animation. */
export const collectAnimationShorthands = (css) =>
  [...stripNonCode(css).matchAll(ANIMATION_SHORTHAND)].flatMap((match) =>
    splitTopLevel(match[1] ?? ''),
  );

/** Every name referenced via the `animation-name` longhand. */
export const collectAnimationNameLonghands = (css) =>
  [...stripNonCode(css).matchAll(ANIMATION_NAME_LONGHAND)]
    .flatMap((match) => splitTopLevel(match[1] ?? ''))
    .filter((name) => !NON_NAME_TOKENS.has(name.toLowerCase()));

/** True when the stylesheet sets an indefinite iteration count via the longhand. */
export const usesInfiniteIterationLonghand = (css) =>
  [...stripNonCode(css).matchAll(ITERATION_COUNT_LONGHAND)].some((match) =>
    splitTopLevel(match[1] ?? '').some((value) => value.toLowerCase() === 'infinite'),
  );

/**
 * Matches any `@keyframes` header and captures its name. The at-rule keyword is
 * case-insensitive per CSS, but the NAME is not, so the name is compared
 * exactly rather than folded into this regex.
 */
const KEYFRAMES_HEADER = /@(?:-webkit-|-moz-|-o-)?keyframes\s+([A-Za-z_-][\w-]*)\s*\{/gi;

/** Extract the body of `@keyframes <name> { ... }`, brace-balanced. */
const keyframeBody = (css, name) => {
  for (const header of css.matchAll(KEYFRAMES_HEADER)) {
    if (header[1] !== name) continue;

    let depth = 0;
    let body = '';
    for (let i = header.index + header[0].length - 1; i < css.length; i += 1) {
      const ch = css.charAt(i);
      if (ch === '{') depth += 1;
      if (depth > 0) body += ch;
      if (ch === '}') {
        depth -= 1;
        if (depth === 0) return body;
      }
    }
  }
  return null;
};

/**
 * Number of gaps between adjacent keyframe offsets.
 *
 * This is what makes the budget arithmetic work: a timing function is applied
 * between EACH pair of adjacent keyframes, not once across the iteration, so a
 * two-segment `0%,100% / 50%` animation ticks `2 * steps` times per cycle.
 *
 * Returns null when there is no such keyframes block, which the caller treats
 * as unverifiable.
 */
export const keyframeSegments = (css, name) => {
  if (name == null) return null;
  // Normalised here rather than at the call site: a `}` inside a comment would
  // otherwise close the block early and silently undercount the segments.
  const body = keyframeBody(stripNonCode(css), name);
  if (body == null) return null;

  const offsets = new Set();
  for (const selector of body.matchAll(/(^|[{}])\s*([^{}]+?)\s*\{/g)) {
    for (const stop of (selector[2] ?? '').split(',')) {
      const token = stop.trim().toLowerCase();
      if (token === 'from') offsets.add(0);
      else if (token === 'to') offsets.add(100);
      else if (/^[\d.]+%$/.test(token)) offsets.add(Number.parseFloat(token));
    }
  }
  // An omitted `from`/`to` still bounds a segment.
  offsets.add(0);
  offsets.add(100);

  return Math.max(1, offsets.size - 1);
};

/** Every `@keyframes` name defined in the stylesheet. */
export const collectKeyframeNames = (css) =>
  new Set(
    [
      ...stripNonCode(css).matchAll(/@(?:-webkit-|-moz-|-o-)?keyframes\s+([A-Za-z_-][\w-]*)/gi),
    ].map((m) => m[1]),
  );
