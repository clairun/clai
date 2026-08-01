import { describe, expect, it } from 'vitest';

import {
  animationNameFrom,
  withoutVarGroups,
  collectAnimationNameLonghands,
  collectAnimationShorthands,
  collectKeyframeNames,
  durationSecondsFrom,
  hasInfinite,
  keyframeSegments,
  splitTopLevel,
  stepCountFrom,
  stripNonCode,
  tokenize,
  usesInfiniteIterationLonghand,
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

describe('durationSecondsFrom', () => {
  it('reads seconds', () => {
    expect(durationSecondsFrom('spin 1.5s linear')).toBe(1.5);
  });

  it('converts milliseconds', () => {
    expect(durationSecondsFrom('spin 250ms linear')).toBe(0.25);
  });

  it('takes the first time, which is the duration and not the delay', () => {
    expect(durationSecondsFrom('spin 2s 5s linear')).toBe(2);
  });

  it('returns null when the duration is behind a custom property', () => {
    expect(durationSecondsFrom('spin var(--fast) steps(4) infinite')).toBeNull();
  });
});

describe('stepCountFrom', () => {
  it('reads a plain step count', () => {
    expect(stepCountFrom('spin 1s steps(18) infinite')).toBe(18);
  });

  it('reads a step count with a jump position', () => {
    expect(stepCountFrom('spin 1s steps(6, jump-start) infinite')).toBe(6);
  });

  it('tolerates whitespace inside steps()', () => {
    expect(stepCountFrom('spin 1s steps(  7 , end ) infinite')).toBe(7);
  });

  it('returns null for a continuous timing function', () => {
    expect(stepCountFrom('spin 1s linear infinite')).toBeNull();
    expect(stepCountFrom('spin 1s cubic-bezier(0,0,1,1) infinite')).toBeNull();
  });

  it('does not treat step-start as a step count', () => {
    expect(stepCountFrom('spin 1s step-start infinite')).toBeNull();
  });

  it('ignores a steps() hidden in a var() fallback, which may never apply', () => {
    expect(stepCountFrom('spin 1s var(--motion-timing, steps(4)) infinite')).toBeNull();
  });

  it('is case-insensitive', () => {
    expect(stepCountFrom('spin 1s STEPS(4) infinite')).toBe(4);
  });
});

describe('hasInfinite', () => {
  it('detects the keyword', () => {
    expect(hasInfinite('spin 1s linear infinite')).toBe(true);
  });

  it('is false for a finite animation', () => {
    expect(hasInfinite('spin 1s linear 3')).toBe(false);
  });

  it('does not match a name that merely contains the word', () => {
    expect(hasInfinite('infiniteScroll 1s linear')).toBe(false);
  });

  it('is case-insensitive, because CSS keywords are', () => {
    expect(hasInfinite('spin 1s linear INFINITE')).toBe(true);
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

describe('withoutVarGroups', () => {
  it('removes a simple var()', () => {
    expect(withoutVarGroups('spin var(--d) linear').replace(/\s+/g, ' ').trim()).toBe(
      'spin linear',
    );
  });

  it('removes a var() with a fallback, including nested parens', () => {
    expect(
      withoutVarGroups('spin 1s var(--t, steps(4)) infinite').replace(/\s+/g, ' ').trim(),
    ).toBe('spin 1s infinite');
  });

  it('leaves a steps() that is not inside a var()', () => {
    expect(withoutVarGroups('spin 1s steps(4) infinite')).toBe('spin 1s steps(4) infinite');
  });

  it('collapses nested var() groups', () => {
    expect(withoutVarGroups('a var(--x, var(--y, steps(4))) b')).toBe('a  b');
  });

  it('does not treat an identifier merely ending in "var" as a var() group', () => {
    expect(withoutVarGroups('--myvar(1) steps(4)')).toBe('--myvar(1) steps(4)');
  });

  it('leaves an identifier that merely contains "var" alone', () => {
    expect(withoutVarGroups('variant 1s steps(4) infinite')).toBe('variant 1s steps(4) infinite');
  });

  it('is case-insensitive', () => {
    expect(withoutVarGroups('a VAR(--x, steps(4)) b')).toBe('a  b');
  });

  it('drops the remainder of an unterminated var(, so nothing false-positives as quantised', () => {
    expect(withoutVarGroups('a var(--x steps(4) infinite')).toBe('a ');
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
});

describe('usesInfiniteIterationLonghand', () => {
  it('detects the longhand form the shorthand check cannot verify', () => {
    expect(
      usesInfiniteIterationLonghand(
        '.a { animation-name: spin; animation-duration: 1s; animation-iteration-count: infinite; }',
      ),
    ).toBe(true);
  });

  it('is false for a finite iteration count', () => {
    expect(usesInfiniteIterationLonghand('.a { animation-iteration-count: 3; }')).toBe(false);
  });

  it('is false when only the shorthand is used', () => {
    expect(usesInfiniteIterationLonghand('.a { animation: spin 1s steps(4) infinite; }')).toBe(
      false,
    );
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

describe('keyframeSegments', () => {
  // This is the arithmetic the whole budget rests on: a timing function is
  // applied between each pair of adjacent keyframes, not once per iteration.
  it('counts a `to`-only block as one segment', () => {
    expect(keyframeSegments('@keyframes spin { to { transform: rotate(360deg); } }', 'spin')).toBe(1);
  });

  it('counts a from/to block as one segment', () => {
    expect(keyframeSegments('@keyframes spin { from { opacity: 0 } to { opacity: 1 } }', 'spin')).toBe(
      1,
    );
  });

  it('counts a 0%/100% block as one segment', () => {
    expect(
      keyframeSegments('@keyframes ripple { 0% { opacity: 1 } 100% { opacity: 0 } }', 'ripple'),
    ).toBe(1);
  });

  it('counts a grouped `0%, 100%` plus `50%` block as two segments', () => {
    expect(
      keyframeSegments('@keyframes pulse { 0%, 100% { opacity: 1 } 50% { opacity: 0.4 } }', 'pulse'),
    ).toBe(2);
  });

  it('counts the four-stop blink cursor as three segments', () => {
    expect(
      keyframeSegments(
        '@keyframes blink { 0%, 49% { opacity: 1 } 50%, 100% { opacity: 0 } }',
        'blink',
      ),
    ).toBe(3);
  });

  it('handles fractional offsets: three stops bound two segments', () => {
    expect(
      keyframeSegments(
        '@keyframes t { 0% { opacity: 0 } 33.3% { opacity: 1 } 100% { opacity: 0 } }',
        't',
      ),
    ).toBe(2);
  });

  it('adds the implicit bounds when only a middle stop is declared', () => {
    expect(keyframeSegments('@keyframes t { 50% { opacity: 0 } }', 't')).toBe(2);
  });

  it('finds a vendor-prefixed block', () => {
    expect(keyframeSegments('@-webkit-keyframes spin { to { opacity: 1 } }', 'spin')).toBe(1);
  });

  it('picks the right block when several are defined', () => {
    const css =
      '@keyframes a { to { opacity: 1 } } @keyframes b { 0%,100% { opacity: 1 } 50% { opacity: 0 } }';
    expect(keyframeSegments(css, 'a')).toBe(1);
    expect(keyframeSegments(css, 'b')).toBe(2);
  });

  it('stops at the end of the block rather than running into the next one', () => {
    const css = '@keyframes a { to { opacity: 1 } } @keyframes b { 25% { opacity: 0 } }';
    // `b`'s 25% stop must not leak into `a`'s segment count.
    expect(keyframeSegments(css, 'a')).toBe(1);
  });

  it('returns null when the block is absent, so the caller can report it', () => {
    expect(keyframeSegments('.a { animation: ghost 1s steps(4) infinite; }', 'ghost')).toBeNull();
  });

  it('returns null for a null name rather than throwing', () => {
    expect(keyframeSegments('@keyframes t { to { opacity: 1 } }', null)).toBeNull();
  });

  it('is not fooled by a closing brace inside a comment', () => {
    // A raw `}` in a comment used to close the block early and undercount.
    expect(
      keyframeSegments(
        '@keyframes t { 0% { opacity: 0 } /* } */ 50% { opacity: .5 } 100% { opacity: 1 } }',
        't',
      ),
    ).toBe(2);
  });

  it('treats keyframe names as case-sensitive, as CSS does', () => {
    expect(keyframeSegments('@keyframes Spin { 0%,50%,100% { opacity: 1 } }', 'spin')).toBeNull();
    expect(keyframeSegments('@keyframes Spin { 0%,50%,100% { opacity: 1 } }', 'Spin')).toBe(2);
  });

  it('still accepts an at-rule keyword in any case, which CSS does allow', () => {
    expect(keyframeSegments('@KEYFRAMES spin { to { opacity: 1 } }', 'spin')).toBe(1);
  });

  it('skips a same-prefix block to find the exact name', () => {
    const css = '@keyframes Spin { 0%,50%,100% { opacity: 1 } } @keyframes spin { to { opacity: 1 } }';
    expect(keyframeSegments(css, 'spin')).toBe(1);
  });

  it('does not confuse a name with a prefix of another name', () => {
    const css = '@keyframes spinFast { 0%,50%,100% { opacity: 1 } } @keyframes spin { to { opacity: 1 } }';
    expect(keyframeSegments(css, 'spin')).toBe(1);
  });
});

describe('budget arithmetic (the regressions this guard exists to catch)', () => {
  const rate = (css, shorthand) => {
    const name = animationNameFrom(shorthand);
    const segments = keyframeSegments(css, name);
    return (segments * stepCountFrom(shorthand)) / durationSecondsFrom(shorthand);
  };

  const pulseKeyframes = '@keyframes pulse { 0%, 100% { opacity: 1 } 50% { opacity: 0.4 } }';
  const spinKeyframes = '@keyframes runSpin { to { transform: rotate(360deg) } }';

  it('scores the shipped two-segment pulse at 16/s', () => {
    expect(rate(pulseKeyframes, 'pulse 1.5s steps(12) infinite')).toBe(16);
  });

  it('scores a two-segment pulse at steps(24) as 32/s, not 16/s', () => {
    // The mistake this catches: reading steps(24) over 1.5s as 16/s by
    // forgetting that the timing function applies per segment.
    expect(rate(pulseKeyframes, 'pulse 1.5s steps(24) infinite')).toBe(32);
  });

  it('scores the shipped one-segment spinner at 16.4/s', () => {
    expect(rate(spinKeyframes, 'runSpin 1.1s steps(18) infinite')).toBeCloseTo(16.36, 2);
  });

  it('scores an inflated step count as over budget', () => {
    expect(rate(spinKeyframes, 'runSpin 1.1s steps(60) infinite')).toBeGreaterThan(18);
  });
});
