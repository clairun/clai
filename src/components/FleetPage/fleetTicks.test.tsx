import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import { useRef } from 'react';
import { Caption } from './FleetScene';
import { BEAT_MS, useThoughtBeat } from './useThoughtBeat';
import styles from './FleetPage.module.css';

beforeEach(() => vi.useFakeTimers({ now: Date.UTC(2026, 9, 18, 12) }));
afterEach(() => vi.useRealTimers());

describe('Fleet ticks', () => {
  it('ages a caption once a minute, from the time of the last event', () => {
    const at = Date.now() - 34 * 60_000;
    render(<Caption caption={{ kind: 'idle', at }} mood="idle" now={Date.now()} />);
    expect(screen.getByText('Last word · 34 min ago')).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(60_000));
    expect(screen.getByText('Last word · 35 min ago')).toBeInTheDocument();
  });

  const Clouds = ({ active }: { active: boolean }) => {
    const ref = useRef<HTMLDivElement | null>(null);
    useThoughtBeat(ref, active);
    return (
      <div ref={ref}>
        <svg className={styles.think} data-testid="cloud" />
      </div>
    );
  };

  it('steps the thought dots only while something works, and not under reduced motion', () => {
    const { rerender } = render(<Clouds active />);
    const cloud = screen.getByTestId('cloud');
    act(() => vi.advanceTimersByTime(BEAT_MS));
    expect(cloud).toHaveAttribute('data-beat', '1');
    act(() => vi.advanceTimersByTime(BEAT_MS));
    expect(cloud).toHaveAttribute('data-beat', '2');
    rerender(<Clouds active={false} />);
    expect(cloud).not.toHaveAttribute('data-beat');

    vi.stubGlobal('matchMedia', (query: string) => ({ matches: query.includes('reduce') }));
    rerender(<Clouds active />);
    act(() => vi.advanceTimersByTime(3 * BEAT_MS));
    expect(cloud).not.toHaveAttribute('data-beat');
    vi.unstubAllGlobals();
  });
});
