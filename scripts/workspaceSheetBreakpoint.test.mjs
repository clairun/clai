// Container queries cannot read custom properties, so the pane width below
// which the drawer and both side panels open over the chat is written in each
// stylesheet. A stale copy would open one panel beside the chat and another
// over it at the same width. Lives in scripts/ because it reads the files
// from disk: src/ is type-checked without Node types, and the vitest config
// stubs CSS imports (`css: false`), `?raw` included.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

const SRC = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../src');
const BREAKPOINT = /@container workspace-body \(max-width: (\d+px)\)/g;

const breakpoints = (file) =>
  [...fs.readFileSync(path.join(SRC, file), 'utf8').matchAll(BREAKPOINT)].map((match) => match[1]);

describe('workspace-body sheet breakpoint', () => {
  it('is the same in the drawer and both side panels', () => {
    const drawer = breakpoints('pages/Workspace.module.css');
    expect(drawer).toHaveLength(1);
    expect(breakpoints('components/WorkspaceFilePreviewPanel.module.css')).toEqual(drawer);
    expect(breakpoints('components/WorkspaceTaskTranscriptPanel.module.css')).toEqual(drawer);
  });
});
