import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../workspace/client', () => ({ setWorkspaceTitle: vi.fn() }));

import { invoke } from '@tauri-apps/api/core';
import { GeneralPage, SchedulePage } from './WorkspacePages';
import type { SectionHandle } from './sectionHandle';
import type { WorkspaceDetails } from '../../generated/bindings';

const mockInvoke = vi.mocked(invoke);

const DETAILS = {
  workspaceId: 'ws-1',
  title: 'Backend',
  rootPath: '/home/u/backend',
  scheduleEnabled: false,
  schedulePaused: false,
  scheduleKind: null,
} as unknown as WorkspaceDetails;

const submit = async (ref: React.RefObject<SectionHandle | null>) => {
  let result: { ok: boolean; error?: string } | undefined;
  await act(async () => { result = await ref.current!.submit(); });
  return result;
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'workspace_host_timezone') return Promise.resolve('Europe/Madrid');
    if (cmd === 'workspace_preview_schedule') return Promise.resolve([Date.now() + 3_600_000]);
    return Promise.resolve(null);
  });
});

describe('GeneralPage', () => {
  it('shows where the workspace lives and rejects an empty title', async () => {
    const user = userEvent.setup();
    const ref = createRef<SectionHandle>();
    render(<GeneralPage ref={ref} workspaceId="ws-1" details={DETAILS} saving={false} />);
    expect(screen.getByText('/home/u/backend')).toBeInTheDocument();

    await user.clear(screen.getByLabelText(/Title/));
    let verdict: { ok: boolean; error?: string } | undefined;
    act(() => { verdict = ref.current!.validate(); });

    expect(verdict).toEqual({ ok: false, error: 'Workspace title cannot be empty.' });
    expect(screen.getByLabelText(/Title/)).toHaveAttribute('aria-invalid', 'true');
  });
});

describe('SchedulePage', () => {
  it('saves an interval schedule once it is switched on', async () => {
    const user = userEvent.setup();
    const ref = createRef<SectionHandle>();
    const onDirtyChange = vi.fn();
    render(<SchedulePage ref={ref} workspaceId="ws-1" details={DETAILS} saving={false} onDirtyChange={onDirtyChange} />);

    await user.click(screen.getByRole('switch', { name: 'Run on a recurring schedule' }));
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);

    expect(await submit(ref)).toEqual({ ok: true });
    expect(mockInvoke).toHaveBeenCalledWith('workspace_set_schedule', {
      workspaceId: 'ws-1',
      kind: { type: 'interval', intervalMinutes: 30 },
    });
  });

  it('switches to cron in the host timezone, previews the next runs and validates the expression', async () => {
    const user = userEvent.setup();
    const ref = createRef<SectionHandle>();
    render(<SchedulePage ref={ref} workspaceId="ws-1" details={{ ...DETAILS, scheduleEnabled: true } as WorkspaceDetails} saving={false} />);

    await user.click(screen.getByRole('button', { name: 'Cron' }));
    expect(screen.getByRole('button', { name: 'Cron' })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByLabelText('Timezone')).toHaveValue('Europe/Madrid');
    await user.click(screen.getByRole('button', { name: 'Weekdays 9am' }));
    expect(screen.getByLabelText('Cron expression')).toHaveValue('0 9 * * 1-5');
    expect(await screen.findByText('in 1h')).toBeInTheDocument();

    await user.clear(screen.getByLabelText('Cron expression'));
    let verdict: { ok: boolean; error?: string } | undefined;
    act(() => { verdict = ref.current!.validate(); });
    expect(verdict).toEqual({ ok: false, error: 'Cron expression is required.' });
    expect(screen.getByRole('alert')).toHaveTextContent('Cron expression is required.');
  });

  it('pauses at once, without going through the save bar', async () => {
    const user = userEvent.setup();
    const onDetailsRefresh = vi.fn();
    const onDirtyChange = vi.fn();
    render(
      <SchedulePage
        ref={createRef()}
        workspaceId="ws-1"
        details={{ ...DETAILS, scheduleEnabled: true, scheduleKind: { type: 'interval', intervalMinutes: 60 } } as WorkspaceDetails}
        saving={false}
        onDirtyChange={onDirtyChange}
        onDetailsRefresh={onDetailsRefresh}
      />
    );
    expect(screen.getByText('Running')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Pause' }));

    expect(mockInvoke).toHaveBeenCalledWith('workspace_set_schedule_paused', { workspaceId: 'ws-1', paused: true });
    expect(onDetailsRefresh).toHaveBeenCalledTimes(1);
    expect(onDirtyChange).not.toHaveBeenCalledWith(true);
  });
  it('keeps an unsaved edit when details are re-fetched with the same schedule', async () => {
    const user = userEvent.setup();
    const ref = createRef<SectionHandle>();
    const onDirtyChange = vi.fn();
    const onDetailsRefresh = vi.fn();
    const saved = (extra: Partial<WorkspaceDetails> = {}) => ({
      ...DETAILS,
      scheduleEnabled: true,
      scheduleKind: { type: 'cron', expression: '0 9 * * *', timezone: 'Europe/Madrid' },
      ...extra,
    }) as WorkspaceDetails;
    const page = (details: WorkspaceDetails) => (
      <SchedulePage ref={ref} workspaceId="ws-1" details={details} saving={false}
        onDirtyChange={onDirtyChange} onDetailsRefresh={onDetailsRefresh} />
    );
    const { rerender } = render(page(saved()));

    const input = screen.getByLabelText('Cron expression');
    await user.clear(input);
    await user.type(input, '0 10 * * *');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);

    // The 5 s poll: same values, new object.
    await act(async () => { rerender(page(saved())); });
    expect(screen.getByLabelText('Cron expression')).toHaveValue('0 10 * * *');

    // Pause refreshes details with only the paused flag changed.
    await user.click(screen.getByRole('button', { name: 'Pause' }));
    expect(onDetailsRefresh).toHaveBeenCalledTimes(1);
    await act(async () => { rerender(page(saved({ schedulePaused: true }))); });
    expect(screen.getByLabelText('Cron expression')).toHaveValue('0 10 * * *');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
  });
});
