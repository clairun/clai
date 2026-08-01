#!/usr/bin/env node
/**
 * Static check on the stylesheets under `src/`. Runs as part of `npm run lint`.
 *
 * ## A CSS Module may only reference keyframes it defines
 *
 * CSS Modules scopes `@keyframes` per file and rewrites `animation-name` to the
 * hashed name -- including when no matching keyframes block exists in that
 * file. Referencing a keyframe defined in another module, or in a global
 * stylesheet, therefore compiles to a dangling name and the animation silently
 * never runs. There is no build warning and no runtime error; the element just
 * sits still.
 *
 * This is not theoretical: `.spinner` in `AssistantChat.module.css` referenced
 * an undefined `spin` and rendered as a motionless ring. That bug is fixed in
 * the same commit that added this check.
 *
 * Only `*.module.css` is checked. A plain global stylesheet is not scoped by
 * CSS Modules, so it may legitimately reference keyframes defined in another
 * global stylesheet.
 *
 * This is a lint pass rather than a vitest test because vitest stubs CSS
 * imports (`css: false` in vitest.config.js), so a test cannot read stylesheet
 * text. The rule itself is pure and lives in `cssAnimationGrammar.mjs`
 * (`undefinedKeyframeReferences`), unit tested in `cssAnimationGrammar.test.mjs`;
 * this file is only the file walker. Its exit codes are covered end to end by
 * `checkCssAnimations.test.mjs`.
 *
 * Known limitation: `composes: x from "./other.module.css"` is not followed.
 * That is sound today because `composes` cannot import `@keyframes` -- a
 * composed selector's `animation-name` is still rewritten with the consuming
 * file's hash -- so the local-definition requirement holds either way.
 *
 * A file that appears to define an @keyframes with a `<string>` name is skipped
 * whole and says so on stderr, because the string's text is gone before the rule sees it
 * and guessing would mean reporting a name that IS defined. Skipping does not
 * fail the lint; the count in the summary line is what makes it noticeable.
 *
 * Exit codes: 0 clean, 1 violations found, 2 nothing to check (misconfiguration).
 */

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

import { isUnverifiable, undefinedKeyframeReferences } from './cssAnimationGrammar.mjs';

const listCssModules = (dir) => {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listCssModules(full));
    else if (entry.name.endsWith('.module.css')) out.push(full);
  }
  return out.sort();
};

const srcDir = path.resolve(process.cwd(), 'src');
if (!fs.existsSync(srcDir)) {
  console.error(`check-css-animations: no src/ directory at ${srcDir}`);
  process.exit(2);
}

const modules = listCssModules(srcDir);
if (modules.length === 0) {
  console.error(`check-css-animations: found no *.module.css under ${srcDir}`);
  process.exit(2);
}

let violations = 0;
let skipped = 0;
for (const file of modules) {
  const relative = path.relative(process.cwd(), file);
  const css = fs.readFileSync(file, 'utf8');

  if (isUnverifiable(css)) {
    skipped += 1;
    console.error(
      `${relative}: skipped -- something here reads as an @keyframes whose name ` +
        `is a <string>. That name is erased before the check runs, so no ` +
        `animation name in this file is verified.`,
    );
    continue;
  }

  for (const name of undefinedKeyframeReferences(css)) {
    violations += 1;
    console.error(
      `${relative}: \`${name}\` has no @keyframes block in this file. CSS Modules ` +
        `rewrites animation-name per file, so a keyframe defined elsewhere ` +
        `compiles to a dangling name and the animation silently never runs.`,
    );
  }
}

if (violations > 0) {
  console.error(
    `\ncheck-css-animations: ${violations} violation(s) across ${modules.length} ` +
      `CSS module(s). See the header of scripts/check-css-animations.mjs.`,
  );
  process.exit(1);
}

console.log(
  `check-css-animations: ${modules.length - skipped} CSS modules OK` +
    (skipped > 0 ? `, ${skipped} skipped` : ''),
);
