import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { bracketMatching, indentOnInput } from '@codemirror/language'
import { searchKeymap } from '@codemirror/search'
import { Compartment, EditorState, type Extension } from '@codemirror/state'
import {
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
  EditorView,
} from '@codemirror/view'
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language'
import { tags } from '@lezer/highlight'

/** Dark-friendly token colors for Markdown source editing. */
const highlightStyle = HighlightStyle.define([
  { tag: tags.heading1, color: '#8db4fa', fontWeight: '600' },
  { tag: tags.heading2, color: '#8db4fa' },
  { tag: tags.heading3, color: '#8db4fa' },
  { tag: [tags.heading4, tags.heading5, tags.heading6], color: '#7ea4e0' },
  { tag: tags.strong, color: '#eaeaea', fontWeight: '600' },
  { tag: tags.emphasis, fontStyle: 'italic' },
  { tag: tags.strikethrough, textDecoration: 'line-through', color: '#6f7077' },
  { tag: tags.link, color: '#6c9ef8' },
  { tag: tags.url, color: '#6c9ef8' },
  { tag: [tags.monospace], color: '#c3a6ff', fontFamily: 'var(--font-mono)' },
  { tag: tags.quote, color: '#9a9ba3', fontStyle: 'italic' },
  { tag: tags.list, color: '#d9a54a' },
  { tag: tags.meta, color: '#6f7077' },
  { tag: tags.processingInstruction, color: '#66b88f' },
  { tag: tags.keyword, color: '#d9a54a' },
])

const extensions = [
  EditorView.contentAttributes.of({ 'aria-label': 'Document source' }),
  lineNumbers(),
  highlightActiveLineGutter(),
  highlightSpecialChars(),
  history(),
  drawSelection(),
  dropCursor(),
  indentOnInput(),
  bracketMatching(),
  rectangularSelection(),
  highlightActiveLine(),
  EditorView.lineWrapping,
  markdown({ base: markdownLanguage }),
  syntaxHighlighting(highlightStyle),
  keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
]

/**
 * The application save hook (Cmd/Ctrl+S inside the editor). The binding is
 * reconfigured through a compartment so the callback identity can change
 * without rebuilding the state.
 */
const saveCompartment = new Compartment()

export function saveKeymap(onSave: () => void): Extension {
  return saveCompartment.of([
    keymap.of([
      {
        key: 'Mod-s',
        preventDefault: true,
        run: () => {
          onSave()
          return true
        },
      },
    ]),
  ])
}

export function createEditorState(
  initialSource: string,
  onSave: () => void,
  onDocChanged: () => void,
): EditorState {
  return EditorState.create({
    doc: initialSource,
    extensions: [
      ...extensions,
      saveKeymap(onSave),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) onDocChanged()
      }),
    ],
  })
}
