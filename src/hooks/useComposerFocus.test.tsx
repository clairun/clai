import { afterEach, describe, expect, it } from 'vitest';
import { act, fireEvent, render } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { useEffect, useRef, useState } from 'react';
import { FOCUS_TERMINAL_EVENT } from '../utils/workspaceUiEvents';
import { useComposerFocus } from './useComposerFocus';

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
  useComposerFocus(workspaceId);
  const [questionOpen, setQuestionOpen] = useState(false);
  return (
    <>
      <p>The agent replied with a long answer.</p>
      {/* Plain content with its own click handler, as a message that opens a panel. */}
      <p onClick={() => setQuestionOpen(true)}>Show the question</p>
      {questionOpen && <div role="dialog" aria-label="Question" />}
      <p onClick={() => document.getElementById('search')?.focus()}>Search for this</p>
      <p onPointerDown={(e) => e.stopPropagation()} onPointerUp={(e) => e.stopPropagation()}>
        A message that keeps its pointer events
      </p>
      <input id="search" aria-label="Search" />
      <select aria-label="Model">
        <option>fast</option>
      </select>
      <div contentEditable aria-label="Notes" />
      <aside aria-label="Artifacts">
        <button type="button">Download artifact</button>
      </aside>
      <button type="button">Open artifacts</button>
      <div tabIndex={-1}>Settings editor</div>
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

describe('useComposerFocus', () => {
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

});

/** The window becomes active, as alt-tab or a click on it does. */
const activate = () => fireEvent.focus(window);
/** Lets a deferred focus, queued for after the click, run. */
const nextTask = () => act(() => new Promise((resolve) => setTimeout(resolve)));

/** A primary press, and focus where WebKitGTK puts it on mousedown for a focusable target. */
const press = async (user: UserEvent, target: HTMLElement, offset?: number) => {
  await user.pointer({ keys: '[MouseLeft>]', target, offset });
  if (target.matches('button, [tabindex]')) target.focus();
};
const release = (user: UserEvent, target: HTMLElement) => user.pointer({ keys: '[/MouseLeft]', target });

const selectText = (node: Node) => {
  const range = document.createRange();
  range.selectNodeContents(node);
  document.getSelection()?.removeAllRanges();
  document.getSelection()?.addRange(range);
};

describe('useComposerFocus on window activation with no press', () => {
  afterEach(() => {
    document.body.querySelectorAll('[data-layer]').forEach((el) => el.remove());
    document.getSelection()?.removeAllRanges();
    fireEvent.blur(window);
  });

  it('focuses the composer when focus was left on <body>', () => {
    const { composer } = setup();
    activate();
    expect(composer).toHaveFocus();
  });

  it.each([['Open artifacts'], ['Download artifact']])('focuses the composer when focus was left on %s', (name) => {
    const { composer, getByRole } = setup();
    getByRole('button', { name }).focus();
    activate();
    expect(composer).toHaveFocus();
  });

  it('focuses the composer when text is selected in the chat', () => {
    const { composer, getByText } = setup();
    selectText(getByText('The agent replied with a long answer.'));
    activate();
    expect(composer).toHaveFocus();
  });

  it.each([['Search'], ['Model'], ['Notes']])('leaves focus in %s', (label) => {
    const { composer, getByLabelText } = setup();
    const field = getByLabelText(label);
    field.focus();
    activate();
    expect(field).toHaveFocus();
    expect(composer).not.toHaveFocus();
  });

  it.each([['dialog'], ['alertdialog'], ['menu']])('leaves the keyboard to an open %s', (role) => {
    const { composer } = setup();
    addLayer(role);
    activate();
    expect(composer).not.toHaveFocus();
  });

  it('leaves the keyboard to an expanded agent question', () => {
    const { composer } = setup();
    addLayer('dialog', 'expanded');
    activate();
    expect(composer).not.toHaveFocus();
  });

  it('focuses the composer past a collapsed agent question', () => {
    const { composer } = setup();
    addLayer('dialog', 'collapsed');
    activate();
    expect(composer).toHaveFocus();
  });
});

describe('useComposerFocus on window activation during a press', () => {
  afterEach(() => {
    document.body.querySelectorAll('[data-layer]').forEach((el) => el.remove());
    document.getSelection()?.removeAllRanges();
    fireEvent.blur(window);
  });

  const CHAT = 'The agent replied with a long answer.';

  it('waits for the release, then focuses the composer after a click on plain content', async () => {
    const { composer, getByText } = setup();
    const user = userEvent.setup();
    const chat = getByText(CHAT);
    await press(user, chat);
    activate();
    expect(composer).not.toHaveFocus();
    await release(user, chat);
    await nextTask();
    expect(composer).toHaveFocus();
  });

  it.each([['Open artifacts'], ['Download artifact'], ['Settings editor']])(
    'leaves %s the focus its press gave it',
    async (text) => {
      const { composer, getByText } = setup();
    const user = userEvent.setup();
      const target = getByText(text);
      await press(user, target);
      activate();
      expect(target).toHaveFocus();
      await release(user, target);
      await nextTask();
      expect(target).toHaveFocus();
      expect(composer).not.toHaveFocus();
    },
  );

  it('keeps the text a drag selects, with the composer left alone mid-press', async () => {
    const { composer, getByText } = setup();
    const user = userEvent.setup();
    const chat = getByText(CHAT);
    await press(user, chat, 4);
    activate();
    expect(composer).not.toHaveFocus();
    await user.pointer([{ target: chat, offset: 9 }, { keys: '[/MouseLeft]', target: chat }]);
    await nextTask();
    expect(document.getSelection()?.toString()).toBe('agent');
    expect(composer).not.toHaveFocus();
  });

  it('focuses the composer after a click on content that stops its pointer events propagating', async () => {
    const { composer, getByText } = setup();
    const user = userEvent.setup();
    const message = getByText('A message that keeps its pointer events');
    await press(user, message);
    activate();
    expect(composer).not.toHaveFocus();
    await release(user, message);
    await nextTask();
    expect(composer).toHaveFocus();
  });

  // These pass with a microtask in place of the next-task deferral too: user-event
  // waits a task after each action. They show the release rechecks after the
  // click, not that the recheck waits for the click.
  it("leaves the keyboard to a dialog the click's own handler opens", async () => {
    const { composer, getByText, getByRole } = setup();
    const user = userEvent.setup();
    const message = getByText('Show the question');
    await press(user, message);
    activate();
    await release(user, message);
    await nextTask();
    expect(getByRole('dialog', { name: 'Question' })).toBeInTheDocument();
    expect(composer).not.toHaveFocus();
  });

  it("leaves the keyboard to a field the click's own handler focuses", async () => {
    const { composer, getByText, getByLabelText } = setup();
    const user = userEvent.setup();
    const message = getByText('Search for this');
    await press(user, message);
    activate();
    await release(user, message);
    await nextTask();
    expect(getByLabelText('Search')).toHaveFocus();
    expect(composer).not.toHaveFocus();
  });

  it('leaves a later click alone', async () => {
    const { composer, getByText } = setup();
    const user = userEvent.setup();
    const chat = getByText(CHAT);
    await press(user, chat);
    activate();
    await release(user, chat);
    await nextTask();
    expect(composer).toHaveFocus();
    await user.click(chat);
    await nextTask();
    expect(composer).not.toHaveFocus();
  });

  it('focuses the composer at once when the press is not the primary button', () => {
    const { composer, getByText } = setup();
    fireEvent.pointerDown(getByText(CHAT), { button: 2 });
    activate();
    expect(composer).toHaveFocus();
  });
});

describe('useComposerFocus after an activation with no press', () => {
  afterEach(() => fireEvent.blur(window));

  // Also the known cost should the activation come before the press of a
  // click: it ends on <body>, and typing brings the composer back.
  it('leaves a later click on the chat alone', async () => {
    const { composer, getByText } = setup();
    activate();
    expect(composer).toHaveFocus();
    await userEvent.click(getByText('The agent replied with a long answer.'));
    await nextTask();
    expect(composer).not.toHaveFocus();
    await userEvent.keyboard('h');
    expect(composer).toHaveValue('h');
  });
});

describe('useComposerFocus press state', () => {
  afterEach(() => fireEvent.blur(window));

  it('forgets a press once released', async () => {
    const { composer, getByText } = setup();
    await userEvent.click(getByText('The agent replied with a long answer.'));
    activate();
    expect(composer).toHaveFocus();
  });

  it('forgets a press when the window is left', () => {
    const { composer, getByText } = setup();
    fireEvent.pointerDown(getByText('The agent replied with a long answer.'), { button: 0 });
    fireEvent.blur(window);
    activate();
    expect(composer).toHaveFocus();
  });

  it('forgets a press the pointer cancels', () => {
    const { composer, getByText } = setup();
    fireEvent.pointerDown(getByText('The agent replied with a long answer.'), { button: 0 });
    fireEvent.pointerCancel(window);
    activate();
    expect(composer).toHaveFocus();
  });

  it.each([
    ['the window is left', () => fireEvent.blur(window)],
    ['the pointer cancels', () => fireEvent.pointerCancel(window)],
  ])('disarms when %s mid-press', async (_, end) => {
    const { composer, getByText } = setup();
    const chat = getByText('The agent replied with a long answer.');
    fireEvent.pointerDown(chat, { button: 0 });
    activate();
    end();
    fireEvent.pointerDown(chat, { button: 0 });
    fireEvent.pointerUp(chat, { button: 0 });
    await nextTask();
    expect(composer).not.toHaveFocus();
  });
});

describe('useComposerFocus once unmounted', () => {
  it('stops listening and drops the queued focus', async () => {
    const HookOnly = () => {
      useComposerFocus(null);
      return null;
    };
    const composer = render(<Composer />).getByLabelText('composer');
    const chat = render(<p>The agent replied.</p>).getByText('The agent replied.');

    // Unmounted mid-press, after the activation armed it.
    let hook = render(<HookOnly />);
    fireEvent.pointerDown(chat, { button: 0 });
    activate();
    hook.unmount();
    fireEvent.pointerUp(chat, { button: 0 });
    await nextTask();
    expect(composer).not.toHaveFocus();

    // Unmounted with the focus after a release still queued.
    hook = render(<HookOnly />);
    fireEvent.pointerDown(chat, { button: 0 });
    activate();
    fireEvent.pointerUp(chat, { button: 0 });
    hook.unmount();
    await nextTask();
    expect(composer).not.toHaveFocus();

    activate();
    await userEvent.keyboard('h');
    expect(composer).not.toHaveFocus();
    expect(composer).toHaveValue('');
    fireEvent.blur(window);
  });
});
