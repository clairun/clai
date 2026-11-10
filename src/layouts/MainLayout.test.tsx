import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router';
import MainLayout from './MainLayout';

const loadSnapshot = vi.hoisted(() => vi.fn(async () => {}));
vi.mock('../fleet/fleetStore', () => ({ useFleetStore: { getState: () => ({ loadSnapshot }) } }));
vi.mock('../assistant', () => ({ useAssistantEvents: () => {} }));
vi.mock('../fleet/useFleetAttentionFeed', () => ({ useFleetAttentionFeed: () => {} }));
vi.mock('../components/TerminalEmulator/TerminalEmulatorWrapper', () => ({ default: () => null }));
vi.mock('../components/Settings/GlobalSettingsHost', () => ({ default: () => null }));

describe('MainLayout attention snapshot', () => {
  it('loads pending attention on workspace boot and refreshes on focus', () => {
    loadSnapshot.mockClear();
    const view = render(<MemoryRouter initialEntries={['/workspace/a']}><Routes>
      <Route element={<MainLayout />}><Route path="/workspace/:id" element={<div>Workspace</div>} /></Route>
    </Routes></MemoryRouter>);
    expect(loadSnapshot).toHaveBeenCalledTimes(1);
    fireEvent.focus(window);
    expect(loadSnapshot).toHaveBeenCalledTimes(2);
    view.unmount();
    fireEvent.focus(window);
    expect(loadSnapshot).toHaveBeenCalledTimes(2);
  });
});
