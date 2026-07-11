import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  isSoftwareRenderer,
  probeWebglRenderer,
  resetWebglProbeCacheForTests,
  shouldUseWebglRenderer,
} from './webglSupport';

// Minimal fake of the WebGL2 surface the probe touches.
interface FakeGlOptions {
  renderer?: string | null;
  hasDebugInfo?: boolean;
  throwOnGetParameter?: boolean;
}

const UNMASKED_RENDERER_WEBGL = 0x9246;
const RENDERER = 0x1f01;

function makeFakeGl({
  renderer = 'NVIDIA GeForce RTX 3060/PCIe/SSE2',
  hasDebugInfo = true,
  throwOnGetParameter = false,
}: FakeGlOptions = {}) {
  const loseContext = vi.fn();
  const gl = {
    RENDERER,
    getExtension: vi.fn((name: string) => {
      if (name === 'WEBGL_debug_renderer_info') {
        return hasDebugInfo ? { UNMASKED_RENDERER_WEBGL } : null;
      }
      if (name === 'WEBGL_lose_context') {
        return { loseContext };
      }
      return null;
    }),
    getParameter: vi.fn((pname: number) => {
      if (throwOnGetParameter) throw new Error('SECURITY_ERR');
      if (pname === UNMASKED_RENDERER_WEBGL || pname === RENDERER) return renderer;
      return null;
    }),
  };
  return { gl, loseContext };
}

function canvasFactoryFor(gl: unknown): () => HTMLCanvasElement {
  return () =>
    ({
      getContext: (kind: string) => (kind === 'webgl2' ? gl : null),
    }) as unknown as HTMLCanvasElement;
}

afterEach(() => {
  resetWebglProbeCacheForTests();
  vi.restoreAllMocks();
});

describe('isSoftwareRenderer', () => {
  it('flags known software rasterizers regardless of case', () => {
    expect(isSoftwareRenderer('Google SwiftShader')).toBe(true);
    expect(isSoftwareRenderer('llvmpipe (LLVM 15.0.7, 256 bits)')).toBe(true);
    expect(isSoftwareRenderer('Mesa/X.org softpipe')).toBe(true);
    expect(isSoftwareRenderer('Software Rasterizer')).toBe(true);
    expect(isSoftwareRenderer('Microsoft Basic Render Driver')).toBe(true);
    expect(isSoftwareRenderer('ANGLE (Software Adapter Direct3D11)')).toBe(true);
  });

  it('does not flag hardware renderer strings', () => {
    expect(isSoftwareRenderer('NVIDIA GeForce RTX 3060/PCIe/SSE2')).toBe(false);
    expect(isSoftwareRenderer('Mesa Intel(R) UHD Graphics 620 (KBL GT2)')).toBe(false);
    expect(isSoftwareRenderer('AMD Radeon RX 6700 XT (radeonsi, navi22)')).toBe(false);
    expect(isSoftwareRenderer('Apple M2')).toBe(false);
    expect(isSoftwareRenderer('ANGLE (NVIDIA, Direct3D11 vs_5_0 ps_5_0)')).toBe(false);
  });
});

describe('probeWebglRenderer', () => {
  it('returns hardware for a hardware renderer string', () => {
    const { gl, loseContext } = makeFakeGl();
    expect(probeWebglRenderer(canvasFactoryFor(gl))).toBe('hardware');
    // Probe context must be released.
    expect(loseContext).toHaveBeenCalledTimes(1);
  });

  it('returns software for llvmpipe', () => {
    const { gl } = makeFakeGl({ renderer: 'llvmpipe (LLVM 15.0.7, 256 bits)' });
    expect(probeWebglRenderer(canvasFactoryFor(gl))).toBe('software');
  });

  it('falls back to RENDERER when the debug-info extension is missing', () => {
    const { gl } = makeFakeGl({ hasDebugInfo: false, renderer: 'Google SwiftShader' });
    expect(probeWebglRenderer(canvasFactoryFor(gl))).toBe('software');
    expect(gl.getParameter).toHaveBeenCalledWith(RENDERER);
  });

  it('returns unavailable when no webgl2 context exists', () => {
    const factory = () =>
      ({ getContext: () => null }) as unknown as HTMLCanvasElement;
    expect(probeWebglRenderer(factory)).toBe('unavailable');
  });

  it('returns unavailable when getContext throws', () => {
    const factory = () =>
      ({
        getContext: () => {
          throw new Error('boom');
        },
      }) as unknown as HTMLCanvasElement;
    expect(probeWebglRenderer(factory)).toBe('unavailable');
  });

  it('returns unknown when the renderer string cannot be read', () => {
    const { gl, loseContext } = makeFakeGl({ throwOnGetParameter: true });
    expect(probeWebglRenderer(canvasFactoryFor(gl))).toBe('unknown');
    // Cleanup still runs when parameter reads throw.
    expect(loseContext).toHaveBeenCalledTimes(1);
  });

  it('returns unknown for an empty renderer string', () => {
    const { gl } = makeFakeGl({ renderer: '' });
    expect(probeWebglRenderer(canvasFactoryFor(gl))).toBe('unknown');
  });
});

describe('shouldUseWebglRenderer', () => {
  function stubDocumentCanvas(gl: unknown) {
    const createElement = vi
      .spyOn(document, 'createElement')
      .mockImplementation(
        () =>
          ({
            getContext: (kind: string) => (kind === 'webgl2' ? gl : null),
          }) as unknown as HTMLCanvasElement
      );
    return createElement;
  }

  it('is true on hardware WebGL2', () => {
    const { gl } = makeFakeGl();
    stubDocumentCanvas(gl);
    expect(shouldUseWebglRenderer()).toBe(true);
  });

  it('is false on a software rasterizer', () => {
    const { gl } = makeFakeGl({ renderer: 'llvmpipe (LLVM 15.0.7, 256 bits)' });
    stubDocumentCanvas(gl);
    expect(shouldUseWebglRenderer()).toBe(false);
  });

  it('is false when WebGL2 is unavailable (jsdom default)', () => {
    // jsdom canvases have no webgl2 context; exercise the real code path.
    expect(shouldUseWebglRenderer()).toBe(false);
  });

  it('memoizes the probe so repeated mounts do not re-probe', () => {
    const { gl } = makeFakeGl();
    const createElement = stubDocumentCanvas(gl);
    expect(shouldUseWebglRenderer()).toBe(true);
    expect(shouldUseWebglRenderer()).toBe(true);
    expect(createElement).toHaveBeenCalledTimes(1);
  });
});
