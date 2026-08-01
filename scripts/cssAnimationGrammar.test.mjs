import { describe, expect, it } from 'vitest';

import {
  animationNameFrom,
  undefinedKeyframeReferences,
  collectAnimationNameLonghands,
  collectAnimationShorthands,
  collectKeyframeNames,
  isUnverifiable,
  splitTopLevel,
  stripNonCode,
  tokenize,
} from './cssAnimationGrammar.mjs';

describe('tokenize', () => {
  it('splits on whitespace', () => {
    expect(tokenize('spin 1s linear infinite')).toEqual(['spin', '1s', 'linear', 'infinite']);
  });

  it('keeps a function call with inner spaces as one token', () => {
    expect(tokenize('spin 1s steps(4, jump-start) infinite')).toEqual([
      'spin',
      '1s',
      'steps(4, jump-start)',
      'infinite',
    ]);
  });

  it('keeps nested parens together', () => {
    expect(tokenize('x cubic-bezier(0.1, 0.7, 1, 0.1) 2s')).toEqual([
      'x',
      'cubic-bezier(0.1, 0.7, 1, 0.1)',
      '2s',
    ]);
  });

  it('collapses runs of whitespace and newlines', () => {
    expect(tokenize('spin\n  1s\t linear')).toEqual(['spin', '1s', 'linear']);
  });
});

describe('splitTopLevel', () => {
  it('splits multiple animations', () => {
    expect(splitTopLevel('spin 1s linear, fade 2s ease')).toEqual(['spin 1s linear', 'fade 2s ease']);
  });

  it('does not split inside steps()', () => {
    expect(splitTopLevel('spin 1s steps(4, jump-end) infinite')).toEqual([
      'spin 1s steps(4, jump-end) infinite',
    ]);
  });

  it('drops empty entries from trailing commas', () => {
    expect(splitTopLevel('spin 1s, ')).toEqual(['spin 1s']);
  });
});

describe('animationNameFrom', () => {
  it('returns null for a <string> name, which is unverifiable once stripped', () => {
    expect(animationNameFrom('"" 1s linear infinite')).toBeNull();
  });

  it('returns a dashed ident, which is a valid custom-ident animation name', () => {
    expect(animationNameFrom('--spin 1s linear infinite')).toBe('--spin');
  });

  it('takes the first non-keyword token', () => {
    expect(animationNameFrom('spin 1s linear infinite')).toBe('spin');
  });

  it('finds the name when it is not written first', () => {
    expect(animationNameFrom('1s linear infinite spin')).toBe('spin');
  });

  it('skips step and bezier timing functions', () => {
    expect(animationNameFrom('2s steps(4) cubic-bezier(0,0,1,1) pulse')).toBe('pulse');
  });

  it('skips a bare iteration count', () => {
    expect(animationNameFrom('1s 3 spin')).toBe('spin');
  });

  it('returns null when the name comes from a custom property', () => {
    expect(animationNameFrom('var(--spin-name) 1s steps(4) infinite')).toBeNull();
  });

  it('returns null when there is no name', () => {
    expect(animationNameFrom('1s linear infinite')).toBeNull();
  });

  it('treats keywords case-insensitively but preserves the name case', () => {
    expect(animationNameFrom('1s LINEAR INFINITE runSpin')).toBe('runSpin');
  });
});

describe('stripNonCode', () => {
  it('removes block comments', () => {
    expect(stripNonCode('a { /* animation: x 1s linear infinite; */ color: red; }')).toBe(
      'a {  color: red; }',
    );
  });

  it('removes multi-line comments', () => {
    expect(stripNonCode('/* line\n * two\n */ a {}')).toBe(' a {}');
  });

  it('empties double-quoted strings but keeps the quotes', () => {
    expect(stripNonCode('a::before { content: "animation: spin 1s linear infinite"; }')).toBe(
      'a::before { content: ""; }',
    );
  });

  it('empties single-quoted strings', () => {
    expect(stripNonCode("a::before { content: 'x'; }")).toBe('a::before { content: ""; }');
  });

  it('keeps an escaped quote from ending the string early', () => {
    expect(stripNonCode('a { content: "a\\"b"; color: red }')).toBe('a { content: ""; color: red }');
  });

  it('does not let a comment opener inside a string start a comment', () => {
    expect(stripNonCode('a { content: "/*"; color: red; }')).toBe('a { content: ""; color: red; }');
  });

  it('does not let a quote inside a comment start a string', () => {
    expect(stripNonCode('a { /* it\'s fine */ color: red; }')).toBe('a {  color: red; }');
  });
});

describe('collectAnimationShorthands', () => {
  it('finds a declaration terminated by a semicolon', () => {
    expect(collectAnimationShorthands('.a { animation: spin 1s linear infinite; }')).toEqual([
      'spin 1s linear infinite',
    ]);
  });

  it('finds a declaration terminated by the closing brace', () => {
    expect(collectAnimationShorthands('.a { animation: spin 1s linear infinite }')).toEqual([
      'spin 1s linear infinite',
    ]);
  });

  it('splits a multi-animation shorthand into separate entries', () => {
    expect(
      collectAnimationShorthands('.a { animation: spin 1s linear infinite, fade 2s ease; }'),
    ).toEqual(['spin 1s linear infinite', 'fade 2s ease']);
  });

  it('finds vendor-prefixed declarations', () => {
    expect(collectAnimationShorthands('.a { -webkit-animation: spin 1s linear infinite; }')).toEqual(
      ['spin 1s linear infinite'],
    );
  });

  it('ignores a custom property that merely ends in "animation"', () => {
    expect(collectAnimationShorthands('.a { --card-animation: spin 1s linear infinite; }')).toEqual(
      [],
    );
  });

  it('ignores the animation-name longhand', () => {
    expect(collectAnimationShorthands('.a { animation-name: spin; }')).toEqual([]);
  });

  it('ignores a declaration that only exists inside a string literal', () => {
    // `content: "animation: ..."` is text, not a declaration. Linting it
    // reported a dangling keyframe name against a file that had none.
    expect(
      collectAnimationShorthands('.x::before { content: "animation: spin 1s linear infinite"; }'),
    ).toEqual([]);
  });

  it('ignores a commented-out declaration', () => {
    expect(collectAnimationShorthands('.a { /* animation: spin 1s linear infinite; */ }')).toEqual(
      [],
    );
  });

  it('finds declarations inside an at-rule', () => {
    expect(
      collectAnimationShorthands(
        '@media (prefers-reduced-motion: no-preference) { .a { animation: spin 1s steps(4) infinite; } }',
      ),
    ).toEqual(['spin 1s steps(4) infinite']);
  });
});

describe('collectAnimationNameLonghands', () => {
  it('drops a var() reference, which is unverifiable rather than missing', () => {
    expect(collectAnimationNameLonghands('.a { animation-name: var(--which); }')).toEqual([]);
  });

  it('drops a var() with a fallback, keeping a real sibling name', () => {
    expect(
      collectAnimationNameLonghands('.a { animation-name: var(--which, spin), fade; }'),
    ).toEqual(['fade']);
  });

  it('collects names', () => {
    expect(collectAnimationNameLonghands('.a { animation-name: spin, fade; }')).toEqual([
      'spin',
      'fade',
    ]);
  });

  it('drops the `none` keyword', () => {
    expect(collectAnimationNameLonghands('.a { animation-name: none; }')).toEqual([]);
  });

  it('is not confused by the animation-iteration-count longhand', () => {
    expect(collectAnimationNameLonghands('.a { animation-iteration-count: infinite; }')).toEqual([]);
  });

  it('checks a keyword-shaped name, which the longhand grammar allows', () => {
    // `animation-name` is `none | <keyframes-name>#`, so unlike the shorthand
    // there is no slot for `ease-out` to fill except the name.
    expect(collectAnimationNameLonghands('.a { animation-name: ease-out; }')).toEqual(['ease-out']);
  });

  it('drops `none` and the CSS-wide keywords', () => {
    for (const keyword of ['none', 'initial', 'inherit', 'unset', 'revert', 'revert-layer']) {
      expect(collectAnimationNameLonghands(`.a { animation-name: ${keyword}; }`)).toEqual([]);
    }
  });
});

describe('collectKeyframeNames', () => {
  it('collects plain and vendor-prefixed blocks', () => {
    const css = '@keyframes spin { to {} } @-webkit-keyframes fade { to {} }';
    expect([...collectKeyframeNames(css)].sort()).toEqual(['fade', 'spin']);
  });

  it('is empty when nothing is defined', () => {
    expect(collectKeyframeNames('.a { color: red; }').size).toBe(0);
  });
});

describe('undefinedKeyframeReferences', () => {
  it('ignores a <string> keyframes name, which stripNonCode erases', () => {
    expect(
      undefinedKeyframeReferences(
        '.a { animation: "spin" 1s linear infinite; }\n@keyframes "spin" { to { opacity: 0; } }',
      ),
    ).toEqual([]);
    expect(undefinedKeyframeReferences('.a { animation-name: "spin"; }')).toEqual([]);
  });

  it('is empty when every reference is defined locally', () => {
    expect(
      undefinedKeyframeReferences(
        '.a { animation: spin 1s linear infinite; }\n@keyframes spin { to { opacity: 0; } }',
      ),
    ).toEqual([]);
  });

  it('reports the historical `.spinner` shape', () => {
    expect(undefinedKeyframeReferences('.spinner { animation: spin 0.8s linear infinite; }')).toEqual(
      ['spin'],
    );
  });

  it('collects from the shorthand and the longhand together, without duplicates', () => {
    expect(
      undefinedKeyframeReferences('.a { animation: one 1s; }\n.b { animation-name: two, one; }'),
    ).toEqual(['two', 'one']);
  });

  it('is case sensitive, because @keyframes names are', () => {
    expect(
      undefinedKeyframeReferences(
        '.a { animation: Spin 1s linear infinite; }\n@keyframes spin { to { opacity: 0; } }',
      ),
    ).toEqual(['Spin']);
  });

  it('ignores a var()-driven name in either position', () => {
    expect(
      undefinedKeyframeReferences(
        '.a { animation-name: var(--x); }\n.b { animation: var(--y) 1s linear infinite; }',
      ),
    ).toEqual([]);
  });

  it('ignores a keyframes block defined inside @media', () => {
    expect(
      undefinedKeyframeReferences(
        '.a { animation: spin 1s linear infinite; }\n@media (min-width: 1px) { @keyframes spin { to { opacity: 0; } } }',
      ),
    ).toEqual([]);
  });
  it('reports a keyword-shaped name referenced through the longhand', () => {
    expect(undefinedKeyframeReferences('.a { animation-name: ease-out; }')).toEqual(['ease-out']);
    expect(undefinedKeyframeReferences('@keyframes ease-out {} .a { animation-name: ease-out; }')).toEqual(
      [],
    );
  });

  it('does not report a keyword-shaped name in the shorthand (documented false negative)', () => {
    // `animation: ease-in ease-out` really does name a keyframe `ease-out`;
    // resolving that needs slot-filling. Under-reporting is the safe direction.
    expect(undefinedKeyframeReferences('.a { animation: ease-in ease-out; }')).toEqual([]);
  });

  it('skips a file whose @keyframes is named with a string, rather than crying wolf', () => {
    // `@keyframes "spin"` defines the same name as `@keyframes spin`, but the
    // string contents are erased before the rule runs, so reporting `spin` as
    // missing would be a false positive on valid CSS.
    expect(undefinedKeyframeReferences('.a { animation: spin 1s; } @keyframes "spin" { }')).toEqual(
      [],
    );
  });

  it('does not report a string-form reference, whose name it cannot see', () => {
    expect(undefinedKeyframeReferences('.a { animation: "missing" 1s linear; }')).toEqual([]);
  });

  it('reports the skip through `isUnverifiable`, so the walker can announce it', () => {
    expect(isUnverifiable('.a { animation: spin 1s; } @keyframes "spin" { }')).toBe(true);
    expect(isUnverifiable('.a { animation: spin 1s; } @keyframes spin { }')).toBe(false);
    expect(isUnverifiable('.a::before { content: "@keyframes "; }')).toBe(false);
  });

  it('is not silenced by a comment or a string that merely mentions @keyframes', () => {
    // The skip above is a loaded gun: if it fired on any stylesheet that talks
    // about `@keyframes` in prose, that file would stop being linted silently.
    const referencesUndefinedSpin = '.b { animation: spin 1s; }';
    for (const decoy of [
      '/* @keyframes "spin" {} */',
      '.a::before { content: "@keyframes "; }',
      '.a::before { content: ""; }',
      '.a::before { content: "@keyframes " "spin"; }',
    ]) {
      expect(undefinedKeyframeReferences(`${decoy} ${referencesUndefinedSpin}`)).toEqual(['spin']);
    }
  });
});
