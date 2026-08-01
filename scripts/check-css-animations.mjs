#!/usr/bin/env node
/**
 * Static checks on the stylesheets under `src/`. Runs as part of `npm run lint`.
 *
 * This is a lint pass rather than a vitest test because vitest stubs CSS
 * imports (`css: false` in vitest.config.js), so a test cannot read stylesheet
 * text. The grammar it depends on is pure and IS unit-tested, in
 * `cssAnimationGrammar.test.mjs`; this file is only the file walker and the
 * budget policy.
 *
 * ## 1. Indefinite animations must be quantised with `steps()`
 *
 * CLAI renders through WebKitGTK, which on Linux composites the window in
 * software. A CPU profile of a streaming run put ~68% of the UI process's
 * cycles inside pixman's SSE2 solid-fill loop and glibc's non-temporal
 * `memset` -- clearing a multi-megabyte surface -- dispatched from a GTK draw
 * signal. That cost is per FRAME and is independent of how much text is
 * streaming (pearson r = +0.02 against token feed rate).
 *
 * A continuously interpolated animation produces a new computed value every
 * frame, so it repaints every frame for as long as it runs -- and
 * `animation: ... infinite` runs for as long as the element is mounted.
 * `steps(n)` holds the computed value between ticks, so the frames in between
 * have nothing to invalidate.
 *
 * The budget matches the existing Linux frame-rate policy in
 * `StreamingMarkdown.tsx` (`TYPEWRITER_LINUX_FRAME_RATE = 18`). It is a
 * ceiling, not a target: exactly 18 updates/sec passes, 18.1 does not.
 *
 * NOTE: the budget is currently applied on every platform, while the typewriter
 * constant it borrows from is gated on `isLinuxRuntime()`. macOS and Windows
 * composite these animations on the GPU and do not need the cap. Scoping this
 * to Linux is deliberate follow-up work, held until a re-profile confirms the
 * quantisation helps at all -- there is no point paying for platform-
 * conditional CSS to protect a win that has not been measured yet.
 *
 * ## 2. A CSS Module may only reference keyframes it defines
 *
 * Unlike rule 1, this applies to every animation, finite or not.
 *
 * CSS Modules scopes `@keyframes` per file and rewrites `animation-name` to the
 * hashed name -- including when no matching keyframes block exists in that
 * file. Referencing a keyframe defined in another module, or in a global
 * stylesheet, therefore compiles to a dangling name and the animation silently
 * never runs. This is not theoretical: `.spinner` in `AssistantChat.module.css`
 * referenced an undefined `spin` and rendered as a motionless ring.
 */

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

import {
  collectAnimationNameLonghands,
  collectAnimationShorthands,
  collectKeyframeNames,
  durationSecondsFrom,
  hasInfinite,
  animationNameFrom,
  keyframeSegments,
  stepCountFrom,
  stripNonCode,
  usesInfiniteIterationLonghand,
} from './cssAnimationGrammar.mjs';

/** Ceiling on computed-value changes per second for an indefinite animation. */
const MAX_UPDATES_PER_SECOND = 18;

const checkQuantised = (css, report) => {
  if (usesInfiniteIterationLonghand(css)) {
    report(
      'sets `animation-iteration-count: infinite` via the longhand. Use the ' +
        '`animation` shorthand instead so the step budget can be verified ' +
        'statically -- the longhand form spreads name, duration, timing ' +
        'function and count across declarations this lint does not correlate.',
    );
  }

  for (const shorthand of collectAnimationShorthands(css)) {
    if (!hasInfinite(shorthand)) continue;

    const steps = stepCountFrom(shorthand);
    if (steps == null) {
      report(
        `\`animation: ${shorthand}\` runs forever with a continuous timing ` +
          `function, so it repaints every frame for as long as the element is ` +
          `mounted. Give it a steps() timing function.`,
      );
      continue;
    }

    // An unresolvable duration or name is reported rather than skipped: a
    // silent skip would turn this lint from an invariant into a convention.
    const duration = durationSecondsFrom(shorthand);
    if (duration == null) {
      report(
        `\`animation: ${shorthand}\` runs forever but has no literal ` +
          `duration, so its update rate cannot be checked. Inline the ` +
          `duration instead of routing it through a custom property.`,
      );
      continue;
    }

    const name = animationNameFrom(shorthand);
    const segments = name == null ? null : keyframeSegments(css, name);
    if (segments == null) {
      report(
        `\`animation: ${shorthand}\` runs forever but its keyframes could not ` +
          `be resolved in this file, so its update rate cannot be checked. ` +
          `A timing function applies between each pair of adjacent keyframes, ` +
          `so the segment count is required to compute the rate.`,
      );
      continue;
    }

    const updatesPerSecond = (segments * steps) / duration;
    if (updatesPerSecond > MAX_UPDATES_PER_SECOND) {
      report(
        `\`animation: ${shorthand}\` ticks ${segments} segment(s) x ${steps} ` +
          `step(s) over ${duration}s. A timing function applies between EACH ` +
          `pair of adjacent keyframes, so that is ` +
          `${updatesPerSecond.toFixed(1)} repaints/s, over the ` +
          `${MAX_UPDATES_PER_SECOND}/s budget.`,
      );
    }
  }
};

/**
 * Known limitation: `composes: x from "./other.module.css"` is not followed.
 * That is sound today because `composes` cannot import `@keyframes` -- a
 * composed selector's `animation-name` is still rewritten with the consuming
 * file's hash -- so the local-definition requirement holds either way.
 */
const checkKeyframesAreLocal = (css, report) => {
  const defined = collectKeyframeNames(css);

  const referenced = new Set(collectAnimationNameLonghands(css));
  for (const shorthand of collectAnimationShorthands(css)) {
    const name = animationNameFrom(shorthand);
    if (name != null) referenced.add(name);
  }

  for (const name of referenced) {
    if (defined.has(name)) continue;
    report(
      `\`${name}\` has no @keyframes block in this file. CSS Modules rewrites ` +
        `animation-name per file, so a keyframe defined elsewhere compiles to ` +
        `a dangling name and the animation silently never runs.`,
    );
  }
};

const listCssFiles = (dir) => {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listCssFiles(full));
    else if (entry.name.endsWith('.css')) out.push(full);
  }
  return out.sort();
};

const srcDir = path.resolve(process.cwd(), 'src');
if (!fs.existsSync(srcDir)) {
  console.error(`check-css-animations: no src/ directory at ${srcDir}`);
  process.exit(2);
}

const files = listCssFiles(srcDir);
if (files.length === 0) {
  console.error('check-css-animations: found no stylesheets to check');
  process.exit(2);
}

let violations = 0;
for (const file of files) {
  const css = stripNonCode(fs.readFileSync(file, 'utf8'));
  const relative = path.relative(process.cwd(), file);
  const report = (message) => {
    violations += 1;
    console.error(`${relative}: ${message}`);
  };

  checkQuantised(css, report);
  if (file.endsWith('.module.css')) checkKeyframesAreLocal(css, report);
}

if (violations > 0) {
  console.error(
    `\ncheck-css-animations: ${violations} violation(s) across ${files.length} ` +
      `stylesheet(s). See the header of scripts/check-css-animations.mjs.`,
  );
  process.exit(1);
}

console.log(`check-css-animations: ${files.length} stylesheets OK`);
