import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

// The backend validates `create_vega_chart` specs against a vendored copy of
// the Vega-Lite JSON schema (src-tauri/embedded/vega-lite-schema.json,
// compiled once in src-tauri/src/assistant/tools/vega_chart.rs). It must
// describe the same version the renderer (the `vega-lite` npm package)
// implements, or the tool accepts specs the chart cannot draw — and vice
// versa. Regenerate the copy when vega-lite is upgraded:
//   node -e "process.stdout.write(JSON.stringify(require('vega-lite/build/vega-lite-schema.json')))" > src-tauri/embedded/vega-lite-schema.json
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const readJson = (path) => JSON.parse(readFileSync(resolve(root, path), 'utf8'));

describe('vendored Vega-Lite schema', () => {
  it('matches the schema shipped by the installed vega-lite package', () => {
    expect(readJson('src-tauri/embedded/vega-lite-schema.json')).toEqual(
      readJson('node_modules/vega-lite/build/vega-lite-schema.json')
    );
  });
});
