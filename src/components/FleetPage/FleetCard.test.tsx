import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { AssistantRun } from '../../generated/bindings';
import { emptyCard, type FleetLive } from '../../fleet/fleetModel';
import FleetCard from './FleetCard';

const live: FleetLive = { runs: {}, tasks: {} };
const base = emptyCard('ws-1', 'Project');
const run = (status: AssistantRun['status'], error: string | null = null): AssistantRun => ({
  id: 'r1', sessionId: 'main', status, trigger: 'user_message', connectionId: 'c',
  protocolId: 'p', modelId: 'm', startedAt: 1n, completedAt: 2n, error,
});
const renderCard = (lastMainRun: AssistantRun | null) => render(
  <FleetCard card={{ ...base, lastMainRun }} feed={null} shouts={new Set()} live={live}
    faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} />
);

describe('Fleet card run failure', () => {
  it('shows the usage-limit detail as an alert', () => {
    renderCard(run('failed', 'Usage limit reached. Try again tomorrow.'));
    expect(screen.getByRole('alert')).toHaveTextContent('Usage limit reached. Try again tomorrow.');
    expect(screen.getByRole('alert')).toHaveAttribute('data-limit');
  });

  it('shows other failures and clears after success or warnings', () => {
    const view = renderCard(run('failed', 'Provider unavailable'));
    expect(screen.getByRole('alert')).toHaveTextContent('Provider unavailable');
    expect(screen.getByRole('alert')).not.toHaveAttribute('data-limit');
    view.rerender(<FleetCard card={{ ...base, lastMainRun: run('completed') }} feed={null} shouts={new Set()} live={live}
      faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    view.rerender(<FleetCard card={{ ...base, lastMainRun: run('completed_with_warnings', 'One tool failed') }} feed={null} shouts={new Set()} live={live}
      faces={{}} now={3} loud={null} onSelect={() => {}} onOpen={() => {}} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});
