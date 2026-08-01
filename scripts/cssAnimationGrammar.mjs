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
 * unit tests. If postcss ever becomes a direct dependency,
 * `collectAnimationShorthands` is the function worth replacing with an AST
 * walk.
 */

/** `none` plus the CSS-wide keywords: never a name, in any animation property. */
const NEVER_A_NAME = new Set([
  'none',
  'initial',
  'inherit',
  'unset',
  'revert',
  'revert-layer',
]);

/**
 * Tokens this lint refuses to read as the name of an `animation` SHORTHAND.
 *
 * Strictly, none of these is impossible: `<keyframes-name>` is `<custom-ident>`,
 * and CSS Values disambiguates the shorthand per slot, so `animation: ease-in
 * ease-out` legitimately means timing-function `ease-in` and name `ease-out`.
 * Resolving that needs a slot-filling parser; skipping the first keyword-shaped
 * token instead costs a FALSE NEGATIVE (a keyword-named keyframe that is missing
 * goes unreported) and never a false positive, which is the right way round for
 * a lint. No stylesheet here names a keyframe after a keyword.
 *
 * The `animation-name` LONGHAND has no such ambiguity -- its grammar is `none |
 * <keyframes-name>#` -- so it filters `NEVER_A_NAME` only and does check
 * `animation-name: ease-out`.
 *
 * Compared lowercased; CSS keywords are case-insensitive.
 */
const NOT_A_SHORTHAND_NAME = new Set([
  ...NEVER_A_NAME,
  'infinite',
  'normal',
  'reverse',
  'alternate',
  'alternate-reverse',
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
]);

/**
 * Matches the `animation` shorthand, optionally vendor-prefixed. The lookbehind
 * keeps it from matching custom properties such as `--card-animation:`, which
 * are not animation declarations and must not be linted as if they were.
 */
const ANIMATION_SHORTHAND = /(?<![\w-])(?:-webkit-|-moz-|-o-)?animation\s*:\s*([^;{}]+)[;}]/gi;
const ANIMATION_NAME_LONGHAND =
  /(?<![\w-])(?:-webkit-|-moz-|-o-)?animation-name\s*:\s*([^;{}]+)[;}]/gi;

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
 *
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
 * A name that was a string literal before `stripNonCode` collapsed it.
 * `<keyframes-name>` is `<custom-ident> | <string>`, so `animation: "spin"` and
 * `@keyframes "spin"` are both valid CSS -- but the string contents are gone by
 * the time we tokenise, so such a name is unverifiable rather than missing.
 * Nothing in this codebase uses the string form.
 */
const isStringified = (token) => token === '""';

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
 * Returns null when there is no verifiable such token -- in particular when the
 * name is behind a `var()` or given as a `<string>`, both of which the caller
 * must treat as unverifiable rather than absent. A dashed ident such as
 * `--spin` IS a valid <custom-ident> animation name and is returned like any
 * other.
 */
export const animationNameFrom = (shorthand) => {
  for (const token of tokenize(shorthand)) {
    if (NOT_A_SHORTHAND_NAME.has(token.toLowerCase())) continue;
    if (isTime(token) || isNumber(token) || isFunction(token)) continue;
    if (isStringified(token)) return null;
    return token;
  }
  return null;
};

/**
 * Every `animation` shorthand in the stylesheet, one entry per comma-separated
 * animation. Each entry is the WHOLE right-hand side (`spin 1s linear
 * infinite`), not just the name: `animationNameFrom` is what reduces it to a
 * single name and drops `var()`, `<time>`, `<number>` and keyword tokens. The
 * longhand collector has to do its own filtering for the same reason, which is
 * why the `var()`/`<string>` guards appear in both places.
 */
export const collectAnimationShorthands = (css) =>
  [...stripNonCode(css).matchAll(ANIMATION_SHORTHAND)].flatMap((match) =>
    splitTopLevel(match[1] ?? ''),
  );

/**
 * Every name referenced via the `animation-name` longhand.
 *
 * The longhand grammar is `none | <keyframes-name>#`, so unlike the shorthand
 * there is nothing to disambiguate: only `none` and the CSS-wide keywords are
 * dropped, and a keyword-shaped name such as `animation-name: ease-out` IS
 * checked.
 *
 * Function-shaped and string-literal values are dropped rather than returned,
 * mirroring `animationNameFrom`: `animation-name: var(--x)` resolves at runtime
 * and a `<string>` name is erased by `stripNonCode`, so neither is verifiable
 * at lint time and neither must be reported as missing.
 */
export const collectAnimationNameLonghands = (css) =>
  [...stripNonCode(css).matchAll(ANIMATION_NAME_LONGHAND)]
    .flatMap((match) => splitTopLevel(match[1] ?? ''))
    .filter(
      (name) => !NEVER_A_NAME.has(name.toLowerCase()) && !isFunction(name) && !isStringified(name),
    );

/**
 * Every `@keyframes` name defined in the stylesheet. The at-rule keyword is
 * case-insensitive per CSS, but the NAME is not, so names are captured with
 * their original case and compared exactly.
 *
 * The name pattern deliberately admits a leading hyphen: per CSS Syntax 3 an
 * identifier may start with `-` followed by a name-start code point or a second
 * `-`, so both `-legacy` and the dashed ident `--spin` are valid names here and
 * must be matched by the same pattern the reference side uses. Over-matching an
 * invalid ident is harmless -- this lint checks that references resolve, it is
 * not a CSS validator.
 */
export const collectKeyframeNames = (css) =>
  new Set(
    [
      ...stripNonCode(css).matchAll(/@(?:-webkit-|-moz-|-o-)?keyframes\s+([A-Za-z_-][\w-]*)/gi),
    ].map((m) => m[1]),
  );

/**
 * A `@keyframes` block named with a `<string>` rather than a `<custom-ident>`.
 * `stripNonCode` has already erased the string's contents by this point, so all
 * that survives is the empty pair of quotes -- the block is known to exist but
 * its name is not knowable.
 *
 * A `content:` string or a comment that merely mentions `@keyframes` cannot trip
 * it, because both are erased first; there are tests for each. A custom property
 * whose payload happens to read `@keyframes ""` DOES trip it, since a custom
 * property's value is an arbitrary token stream that no regex can tell from real
 * CSS -- the same exposure `collectKeyframeNames` already has to a custom
 * property that spells out a keyframe name. That is why the walker announces
 * every skip: an unexpected one is visible in the lint output rather than
 * quietly turning the file's checking off.
 */
const STRING_KEYFRAMES_HEADER = /@(?:-webkit-|-moz-|-o-)?keyframes\s+""/i;

/**
 * Whether `undefinedKeyframeReferences` will decline to check this stylesheet
 * at all. Exported so the walker can say so out loud: a silent opt-out is the
 * one failure mode of that design, and a line on stderr makes it visible in the
 * lint output instead of leaving it to be discovered by experiment.
 */
export const isUnverifiable = (css) => STRING_KEYFRAMES_HEADER.test(stripNonCode(css));

/**
 * Animation names a CSS Module references but does not define.
 *
 * CSS Modules scopes `@keyframes` per file and rewrites `animation-name` to the
 * file's hashed name whether or not a matching block exists, so a reference to
 * a keyframe defined elsewhere compiles to a dangling name and the animation
 * silently never runs.
 *
 * A file containing a string-named `@keyframes` is skipped entirely (returns
 * `[]`). `<keyframes-name>` is `<custom-ident> | <string>` and the two forms are
 * equivalent, so `@keyframes "spin"` really does define the name `spin` -- but
 * `stripNonCode` erased that name before this ran, and reporting `spin` as
 * missing purely because it was defined in quotes would be a false positive on
 * valid CSS. Giving up on the file trades that for a false negative, which is
 * the safe direction. No stylesheet here uses the string form.
 *
 * Pure so it can be unit tested; `check-css-animations.mjs` supplies the files.
 */
export const undefinedKeyframeReferences = (css) => {
  if (isUnverifiable(css)) return [];

  const defined = collectKeyframeNames(css);

  const referenced = new Set(collectAnimationNameLonghands(css));
  for (const shorthand of collectAnimationShorthands(css)) {
    const name = animationNameFrom(shorthand);
    if (name != null) referenced.add(name);
  }

  return [...referenced].filter((name) => !defined.has(name));
};
