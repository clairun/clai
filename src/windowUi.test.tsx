import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { WindowUiProvider, useWindowUi } from './windowUi';

const Controls = ({ name }: { name: string }) => {
  const { railHidden, toggleRail } = useWindowUi();
  return <div aria-label={name}>
    <span>{railHidden ? 'hidden' : 'shown'}</span>
    <button type="button" onClick={toggleRail}>Toggle</button>
  </div>;
};

describe('window UI state', () => {
  it('starts a workspace launch hidden and keeps two windows independent', () => {
    window.history.replaceState({}, '', '/?workspace=alpha');
    render(<>
      <WindowUiProvider><Controls name="first window" /></WindowUiProvider>
      <WindowUiProvider><Controls name="second window" /></WindowUiProvider>
    </>);
    const first = within(screen.getByLabelText('first window'));
    const second = within(screen.getByLabelText('second window'));
    expect(first.getByText('hidden')).toBeInTheDocument();
    expect(second.getByText('hidden')).toBeInTheDocument();
    fireEvent.click(first.getByRole('button', { name: 'Toggle' }));
    expect(first.getByText('shown')).toBeInTheDocument();
    expect(second.getByText('hidden')).toBeInTheDocument();
    window.history.replaceState({}, '', '/');
  });

  it('shows the rail for a general launch', () => {
    window.history.replaceState({}, '', '/');
    render(<WindowUiProvider><Controls name="general window" /></WindowUiProvider>);
    expect(within(screen.getByLabelText('general window')).getByText('shown')).toBeInTheDocument();
  });
});
