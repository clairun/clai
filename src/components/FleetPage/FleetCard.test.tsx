import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import type { AssistantRun } from '../../generated/bindings';
import { emptyCard, type FleetLive } from '../../fleet/fleetModel';
import FleetCard from './FleetCard';

const live: FleetLive = { runs: {}, tasks: {} };
const base = emptyCard('ws-1', 'Project');
const run = (
  status: AssistantRun['status'], error: string | null = null, errorKind?: AssistantRun['errorKind'],
): AssistantRun => ({
  id: 'r1', sessionId: 'main', status, trigger: 'user_message', connectionId: 'c',
  protocolId: 'p', modelId: 'm', startedAt: 1n, completedAt: 2n, error, errorKind,
});
const renderCard = (lastMainRun: AssistantRun | null) => render(
  <FleetCard card={{ ...base, lastMainRun }} feed={null} shouts={new Set()} live={live}
    faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} onOpenWindow={() => {}} onDismiss={() => {}} />
);

describe('Fleet card run failure', () => {
  it('shows a usage limit as a wait, with what to do', () => {
    renderCard(run('failed', 'Usage limit reached. Your limit will reset at 14:00', 'usage_limit'));
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('Usage limit reached. Your limit will reset at 14:00');
    expect(alert).toHaveTextContent('Wait for it to reset');
    expect(alert).toHaveTextContent('⏳');
    expect(alert).toHaveAttribute('data-kind', 'usage_limit');
  });

  it('shows the action for other kinds without the usage-limit wait', () => {
    renderCard(run('failed', 'The provider rejected the API key.', 'auth'));
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('Update the key in Settings → AI Provider.');
    expect(alert).not.toHaveTextContent('⏳');
  });

  it('marks a failure by its kind, not by limit words in its text', () => {
    renderCard(run('failed', 'Rate limit reached for requests', 'transient'));
    expect(screen.getByRole('alert')).toHaveAttribute('data-kind', 'transient');
    expect(screen.getByRole('alert')).not.toHaveTextContent('⏳');
  });

  it('shows other failures and clears after success or warnings', () => {
    const view = renderCard(run('failed', 'Provider unavailable'));
    expect(screen.getByRole('alert')).toHaveTextContent('Provider unavailable');
    expect(screen.getByRole('alert')).not.toHaveAttribute('data-kind');
    expect(screen.getByRole('alert')).toHaveTextContent(/^⚠Provider unavailable$/);
    view.rerender(<FleetCard card={{ ...base, lastMainRun: run('completed') }} feed={null} shouts={new Set()} live={live}
      faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} onOpenWindow={() => {}} onDismiss={() => {}} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    view.rerender(<FleetCard card={{ ...base, lastMainRun: run('completed_with_warnings', 'One tool failed') }} feed={null} shouts={new Set()} live={live}
      faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} onOpenWindow={() => {}} onDismiss={() => {}} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('moves its words one line a wheel notch, once it shows', () => {
    const view = renderCard(run('completed'));
    view.rerender(<FleetCard card={{ ...base, lastMainRun: run('failed', 'Provider unavailable') }} feed={null} shouts={new Set()} live={live}
      faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} onOpenWindow={() => {}} onDismiss={() => {}} />);
    const banner = screen.getByRole('alert');
    banner.style.lineHeight = '17px';
    const notch = new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: 93 });
    Object.defineProperty(notch, 'wheelDeltaY', { value: -120 });
    expect(banner.dispatchEvent(notch)).toBe(false);
    expect(banner.scrollTop).toBe(17);
  });
});

describe('Fleet card icon', () => {
  it('opens the workspace in a new window, not this one, and leaves the card unselected', () => {
    const onOpen = vi.fn();
    const onOpenWindow = vi.fn();
    const onSelect = vi.fn();
    render(<FleetCard card={base} feed={null} shouts={new Set()} live={live} faces={{}} now={3} loud={null}
      onSelect={onSelect} onOpen={onOpen} onOpenWindow={onOpenWindow} onDismiss={() => {}} />);
    const icon = screen.getByRole('button', { name: 'Open Project in new window' });
    expect(icon).toHaveAttribute('title', 'Open in new window');
    fireEvent.click(icon);
    expect(onOpenWindow).toHaveBeenCalledExactlyOnceWith('ws-1');
    expect(onOpen).not.toHaveBeenCalled();
    expect(onSelect).not.toHaveBeenCalled();
  });
});
