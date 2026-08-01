/**
 * End-to-end tests for the `check-css-animations.mjs` CLI: the file walk, the
 * `*.module.css` filter, the message wording, and the exit codes.
 *
 * The rule itself is unit tested in `cssAnimationGrammar.test.mjs`; this file
 * exists because the walker and the exit-code mapping are only reachable by
 * running the script, and `npm run lint` depends on both.
 */

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

import { afterEach, describe, expect, it } from 'vitest';

const SCRIPT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), 'check-css-animations.mjs');

const tempDirs = [];

/** Build a throwaway project root; `files` maps a path under it to contents. */
const project = (files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'check-css-animations-'));
  tempDirs.push(root);
  for (const [relative, contents] of Object.entries(files)) {
    const full = path.join(root, relative);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, contents);
  }
  return root;
};

/**
 * Run the checker in `cwd`, returning its exit code and merged output.
 *
 * Spawned rather than exec'd so that a non-zero exit is data rather than an
 * exception, and so stderr is captured on EVERY path -- the skip notice is
 * written to stderr on an otherwise successful run, and inherited stderr would
 * both hide it from assertions and spray it over the test report.
 */
const run = (cwd) => {
  const { status, stdout, stderr } = spawnSync(process.execPath, [SCRIPT], {
    cwd,
    encoding: 'utf8',
  });
  return { code: status, output: `${stdout ?? ''}${stderr ?? ''}` };
};

afterEach(() => {
  for (const dir of tempDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

describe('check-css-animations CLI', () => {
  it('passes when every referenced keyframe is defined locally', () => {
    const root = project({
      'src/a.module.css': '.spin { animation: spin 1s linear infinite; }\n@keyframes spin { to { transform: rotate(1turn); } }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(0);
    expect(output).toContain('1 CSS modules OK');
  });

  it('fails on the historical `.spinner` bug shape and names the keyframe', () => {
    const root = project({
      'src/components/AssistantChat/AssistantChat.module.css':
        '.spinner { animation: spin 0.8s linear infinite; }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(1);
    expect(output).toContain(
      path.join('src', 'components', 'AssistantChat', 'AssistantChat.module.css'),
    );
    expect(output).toContain('`spin` has no @keyframes block in this file');
    expect(output).toContain('1 violation(s)');
  });

  it('skips a file with a string-named @keyframes, says so, and stays green', () => {
    // The rule declines to check such a file at all. Announcing it is the point:
    // a silent whole-file opt-out is the one way this lint could stop working
    // without anybody noticing.
    const root = project({
      'src/quoted.module.css': '.a { animation: spin 1s; }\n@keyframes "spin" { from { } }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(0);
    expect(output).toContain('src/quoted.module.css: skipped');
    expect(output).toContain('0 CSS modules OK, 1 skipped');
    expect(output).not.toContain('`spin` has no @keyframes block');
  });

  it('announces the skip even when a custom property tripped it by accident', () => {
    // A custom property's value is an arbitrary token stream, so one that reads
    // like a string-named @keyframes header is indistinguishable from the real
    // thing. It cannot be prevented -- but it must not be silent, or the file
    // stops being checked and nobody finds out.
    const root = project({
      'src/decoy.module.css':
        '.a { --note: @keyframes "not-real"; animation: missing 1s linear infinite; }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(0);
    expect(output).toContain('src/decoy.module.css: skipped');
    expect(output).toContain('0 CSS modules OK, 1 skipped');
  });

  it('walks nested directories and reports every offending file', () => {
    const root = project({
      'src/a.module.css': '.a { animation: one 1s linear; }\n',
      'src/deep/nested/b.module.css': '.b { animation-name: two; }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(1);
    expect(output).toContain('`one` has no @keyframes block');
    expect(output).toContain('`two` has no @keyframes block');
    expect(output).toContain('2 violation(s)');
  });

  it('ignores non-module stylesheets, which CSS Modules does not scope', () => {
    const root = project({
      'src/global.css': '.g { animation: defined-elsewhere 1s linear infinite; }\n',
      'src/a.module.css': '@keyframes only { to { opacity: 0; } }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(0);
    expect(output).toContain('1 CSS modules OK');
  });

  it('does not report a `var()`-driven animation name as missing', () => {
    const root = project({
      'src/a.module.css':
        '.a { animation-name: var(--which); }\n.b { animation: var(--which) 1s linear infinite; }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(0);
    expect(output).not.toContain('var(');
  });

  it('does not treat a commented-out or stringified declaration as a reference', () => {
    const root = project({
      'src/a.module.css':
        '/* .old { animation: gone 1s linear infinite; } */\n.a::after { content: "animation: alsogone 1s linear infinite"; }\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(0);
    expect(output).not.toContain('gone');
  });

  it('exits 2 when there is no src/ directory', () => {
    const { code, output } = run(project({ 'package.json': '{}\n' }));
    expect(code).toBe(2);
    expect(output).toContain('no src/ directory');
  });

  it('exits 2 when src/ holds no CSS modules, rather than passing vacuously', () => {
    const root = project({ 'src/index.ts': 'export {};\n', 'src/global.css': '.a { color: red; }\n' });
    const { code, output } = run(root);
    expect(code).toBe(2);
    expect(output).toContain('found no *.module.css');
  });

  it('handles CRLF line endings', () => {
    const root = project({
      'src/a.module.css': '.a {\r\n  animation: spin 1s linear infinite;\r\n}\r\n',
    });
    const { code, output } = run(root);
    expect(code).toBe(1);
    expect(output).toContain('`spin` has no @keyframes block');
  });
});
