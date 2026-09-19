import { useRef, useState } from 'react'
import { SendHorizontal } from 'lucide-react'

import { useNotes, useAppendToday, useTags } from '@/queries/workspace'

export type CompletionKind = 'tag' | 'wiki'

export interface ActiveCompletion {
  kind: CompletionKind
  /** Index of the trigger character (`#` or the `[` of `[[`) in the text. */
  triggerStart: number
  /** Text typed after the trigger so far. */
  query: string
}

const MAX_ITEMS = 8

/**
 * Detect the completion context at the caret: `#tag` or `[[wikilink`.
 * Returns null when the caret is not inside an active trigger.
 */
export function detectCompletion(text: string, caret: number): ActiveCompletion | null {
  const before = text.slice(0, caret)
  const wikiStart = before.lastIndexOf('[[')
  if (wikiStart !== -1) {
    const between = before.slice(wikiStart + 2)
    const lastClose = before.lastIndexOf(']]')
    if ((lastClose === -1 || lastClose < wikiStart) && !/[\]\n]/.test(between)) {
      return { kind: 'wiki', triggerStart: wikiStart, query: between }
    }
  }
  const tagMatch = /(?:^|\s)#([^\s#]*)$/.exec(before)
  if (tagMatch !== null) {
    const hashIndex = before.length - tagMatch[0].length + tagMatch[0].indexOf('#')
    return { kind: 'tag', triggerStart: hashIndex, query: tagMatch[1] ?? '' }
  }
  return null
}

/**
 * Weibo-style quick input at the top of the daily river: multi-line
 * (Shift+Enter for newlines, Enter to send) with `#tag` and `[[wikilink`
 * completion popups, IME-safe.
 */
export function DailyQuickInput() {
  const appendToday = useAppendToday()
  const tagsQuery = useTags()
  const notesQuery = useNotes()
  const [text, setText] = useState('')
  const [completion, setCompletion] = useState<ActiveCompletion | null>(null)
  const [activeIndex, setActiveIndex] = useState(0)
  const inputRef = useRef<HTMLTextAreaElement | null>(null)

  const tagNames = (tagsQuery.data ?? []).map((tag) => tag.name)
  const noteNames = (notesQuery.data ?? [])
    .filter((note) => !note.archived)
    .map((note) => note.name)

  const items: string[] = (() => {
    if (completion === null) return []
    const query = completion.query.toLowerCase()
    const source = completion.kind === 'tag' ? tagNames : noteNames
    const matches = source.filter((name) => name.toLowerCase().includes(query) && name !== query)
    return matches.slice(0, MAX_ITEMS)
  })()
  const open = completion !== null && items.length > 0

  const handleChange = (value: string) => {
    setText(value)
    const caret = inputRef.current?.selectionStart ?? value.length
    const next = detectCompletion(value, caret)
    setCompletion(next)
    setActiveIndex(0)
  }

  const accept = (item: string) => {
    if (completion === null) return
    const caret = inputRef.current?.selectionStart ?? text.length
    const replacement =
      completion.kind === 'tag' ? `#${item} ` : `[[${item}]]`
    const before = text.slice(0, completion.triggerStart)
    const after = text.slice(caret)
    const next = `${before}${replacement}${after}`
    setText(next)
    setCompletion(null)
    requestAnimationFrame(() => {
      const el = inputRef.current
      if (el !== null) {
        el.focus()
        const caret2 = before.length + replacement.length
        el.setSelectionRange(caret2, caret2)
      }
    })
  }

  const send = () => {
    const body = text.trim()
    if (body === '' || appendToday.isPending) return
    appendToday.mutate(body, {
      onSuccess: () => {
        setText('')
        setCompletion(null)
      },
    })
    inputRef.current?.focus()
  }

  return (
    <div className="relative mb-4 rounded-md border border-border-strong bg-panel px-2.5 py-1.5 focus-within:border-accent">
      {open && (
        <ul
          className="absolute left-0 top-full z-20 mt-1 max-h-48 w-full overflow-y-auto rounded-md border border-border-strong bg-overlay py-1 shadow-lg"
          data-testid="completion-popup"
          role="listbox"
        >
          {items.map((item, index) => (
            <li key={item}>
              <button
                type="button"
                role="option"
                aria-selected={index === activeIndex}
                onMouseDown={(event) => {
                  // mousedown: keep textarea focus/selection intact.
                  event.preventDefault()
                  accept(item)
                }}
                className={`flex w-full items-center gap-2 px-2.5 py-1 text-left text-[12px] transition-colors ${
                  index === activeIndex ? 'bg-accent/20 text-accent-strong' : 'text-fg hover:bg-raised'
                }`}
              >
                <span className="font-mono text-[10px] text-faint">
                  {completion?.kind === 'tag' ? '#' : '[['}
                </span>
                <span className="truncate">{item}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <textarea
        ref={inputRef}
        value={text}
        placeholder="今天有什么想记下来的"
        data-testid="daily-quick-input"
        onChange={(event) => handleChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
          if (open) {
            if (event.key === 'ArrowDown') {
              event.preventDefault()
              setActiveIndex((index) => (index + 1) % items.length)
              return
            }
            if (event.key === 'ArrowUp') {
              event.preventDefault()
              setActiveIndex((index) => (index - 1 + items.length) % items.length)
              return
            }
            if (event.key === 'Tab' || event.key === 'Enter') {
              event.preventDefault()
              accept(items[activeIndex] as string)
              return
            }
            if (event.key === 'Escape') {
              event.preventDefault()
              setCompletion(null)
              return
            }
          }
          if (event.key === 'Enter' && !event.shiftKey) {
            event.preventDefault()
            send()
          }
        }}
        onBlur={() => {
          // Allow the mousedown-accept path to run first.
          window.setTimeout(() => setCompletion(null), 120)
        }}
        className="max-h-48 w-full resize-none overflow-y-auto field-sizing-content bg-transparent text-[13px] text-fg outline-none placeholder:text-faint"
      />
      <div className="flex justify-end">
        <button
          type="button"
          onClick={send}
          disabled={text.trim() === '' || appendToday.isPending}
          className="inline-flex h-6 items-center gap-1 rounded-sm border border-accent/60 bg-accent/20 px-2 text-[11px] font-medium text-accent-strong transition-colors hover:bg-accent/30 disabled:cursor-not-allowed disabled:opacity-50"
        >
          <SendHorizontal size={11} />
          {appendToday.isPending ? 'Sending…' : 'Send'}
        </button>
      </div>
    </div>
  )
}
