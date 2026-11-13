import { afterEach, describe, expect, it } from 'vitest';
import { fireEvent, render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect, useRef } from 'react';
import { FOCUS_TERMINAL_EVENT } from '../utils/workspaceUiEvents';
import { useComposerTypeToFocus } from './useComposerTypeToFocus';

// The composer takes the keyboard on FOCUS_TERMINAL_EVENT, as TerminalEmulator's does.
const Composer = () => {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const focus = () => ref.current?.focus({ preventScroll: true });
    window.addEventListener(FOCUS_TERMINAL_EVENT, focus);
    return () => window.removeEventListener(FOCUS_TERMINAL_EVENT, focus);
  }, []);
  return <textarea ref={ref} aria-label="composer" />;
};

const Host = ({ workspaceId }: { workspaceId: string | null }) => {
  useComposerTypeToFocus(workspaceId);
  return (
    <>
      <p>The agent replied with a long answer.</p>
      <input aria-label="Search" />
      <Composer />
    </>
  );
};

const setup = (workspaceId: string | null = 'a') => {
  const view = render(<Host workspaceId={workspaceId} />);
  const composer = view.getByLabelText('composer');
  // Start where a window the user clicked into is: nothing focused.
  (document.activeElement as HTMLElement | null)?.blur();
  return { ...view, composer };
};

const addLayer = (role: string, agentQuestion?: 'collapsed' | 'expanded') => {
  const layer = document.createElement('div');
  layer.setAttribute('role', role);
  if (agentQuestion) layer.dataset.agentQuestion = agentQuestion;
  layer.dataset.layer = '';
  document.body.append(layer);
};

describe('useComposerTypeToFocus', () => {
  afterEach(() => {
    document.body.querySelectorAll('[data-layer]').forEach((el) => el.remove());
  });

  it('focuses the composer when a workspace is entered with nothing focused', () => {
    const { composer, rerender } = setup(null);
    rerender(<Host workspaceId="b" />);
    expect(composer).toHaveFocus();
  });

  it('does not take focus from an open layer when a workspace is entered', () => {
    const { composer, rerender } = setup(null);
    addLayer('dialog');
    rerender(<Host workspaceId="b" />);
    expect(composer).not.toHaveFocus();
  });

  it('types the first character into the composer when nothing is focused', async () => {
    const { composer } = setup();
    await userEvent.keyboard('hi');
    expect(composer).toHaveFocus();
    expect(composer).toHaveValue('hi');
  });

  it('leaves a key typed in another control there', async () => {
    const { composer, getByLabelText } = setup();
    const search = getByLabelText('Search');
    search.focus();
    await userEvent.keyboard('h');
    expect(search).toHaveValue('h');
    expect(composer).toHaveValue('');
  });

  it.each([['dialog'], ['alertdialog'], ['menu']])('leaves the keyboard to an open %s', async (role) => {
    const { composer } = setup();
    addLayer(role);
    await userEvent.keyboard('h');
    expect(composer).not.toHaveFocus();
    expect(composer).toHaveValue('');
  });

  it('types a character Windows sends as Ctrl+Alt for AltGr', () => {
    const { composer } = setup();
    // user-event cannot set AltGraph; this is WebView2's keydown for AltGr+2.
    fireEvent.keyDown(document.body, { key: '@', ctrlKey: true, altKey: true, modifierAltGraph: true });
    expect(composer).toHaveFocus();
  });

  it('types past a collapsed agent question', async () => {
    const { composer } = setup();
    addLayer('dialog', 'collapsed');
    await userEvent.keyboard('h');
    expect(composer).toHaveValue('h');
  });

  it('types past a question collapsed from its own button', async () => {
    const { composer } = setup();
    const question = document.createElement('section');
    question.setAttribute('role', 'dialog');
    question.dataset.layer = '';
    question.dataset.agentQuestion = 'collapsed';
    const expand = document.createElement('button');
    expand.textContent = 'Expand question';
    question.append(expand);
    document.body.append(question);
    expand.focus();
    await userEvent.keyboard('h');
    expect(composer).toHaveValue('h');
  });

  it('leaves the keyboard to an expanded agent question', async () => {
    const { composer } = setup();
    addLayer('dialog', 'expanded');
    await userEvent.keyboard('h');
    expect(composer).not.toHaveFocus();
    expect(composer).toHaveValue('');
  });

  it.each([['{Control>}h{/Control}'], ['{Meta>}h{/Meta}'], ['{Alt>}h{/Alt}']])('ignores the shortcut %s', async (keys) => {
    const { composer } = setup();
    await userEvent.keyboard(keys);
    expect(composer).not.toHaveFocus();
  });

  it('leaves Space to page the scroller', async () => {
    const { composer } = setup();
    await userEvent.keyboard(' ');
    expect(composer).not.toHaveFocus();
  });

  it('leaves named keys to the page', async () => {
    const { composer } = setup();
    await userEvent.keyboard('{PageDown}{ArrowDown}{Enter}');
    expect(composer).not.toHaveFocus();
  });

  it('takes Shift for capitals', async () => {
    const { composer } = setup();
    await userEvent.keyboard('{Shift>}H{/Shift}');
    expect(composer).toHaveValue('H');
  });

  it('leaves a key some handler already used', async () => {
    const { composer } = setup();
    const claim = (e: KeyboardEvent) => e.preventDefault();
    document.addEventListener('keydown', claim);
    try {
      await userEvent.keyboard('h');
    } finally {
      document.removeEventListener('keydown', claim);
    }
    expect(composer).not.toHaveFocus();
  });

  it('leaves a key that is part of an IME composition', () => {
    const { composer } = setup();
    // user-event cannot compose; this is the event an IME sends.
    fireEvent.keyDown(document.body, { key: 'h', isComposing: true });
    expect(composer).not.toHaveFocus();
  });

  it('stops listening once unmounted', async () => {
    const HookOnly = () => {
      useComposerTypeToFocus('a');
      return null;
    };
    const hook = render(<HookOnly />);
    const composer = render(<Composer />).getByLabelText('composer');
    composer.blur();
    hook.unmount();
    await userEvent.keyboard('h');
    expect(composer).toHaveValue('');
  });
});
