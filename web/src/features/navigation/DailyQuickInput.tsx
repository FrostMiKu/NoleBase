import { useId, useRef, useState } from 'react'
import { SendHorizontal } from 'lucide-react'

import { useNotes, useAppendToday, useTags, useDailyNotes } from '@/queries/workspace'

import { useSelectionViewport } from '@/lib/selection-viewport'
import { useIsMutating } from '@tanstack/react-query'
import { workspaceKeys } from '@/queries/keys'
import { clearSubmittedDraft, useDraft } from '@/hooks/use-draft'

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
    const prefix = before.slice(0, wikiStart)
    if (prefix.endsWith('\\') || prefix.lastIndexOf('[[') > prefix.lastIndexOf(']]')) return null
    const between = before.slice(wikiStart + 2)
    const lastClose = before.lastIndexOf(']]')
    if ((lastClose === -1 || lastClose < wikiStart) && !/[\[\]\n]/.test(between)) {
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
  const sending = useIsMutating({ mutationKey: workspaceKeys.appendToday() }) > 0
  const tagsQuery = useTags()
  const notesQuery = useNotes()
  const dailyQuery = useDailyNotes()
  const [text, setText] = useDraft('daily')
  const popupId = useId()
  const [completion, setCompletion] = useState<ActiveCompletion | null>(null)
  const inputRef = useRef<HTMLTextAreaElement | null>(null)

  const tagNames = (tagsQuery.data ?? []).map((tag) => tag.name)
  const noteNames = [
    ...new Set([
      ...(notesQuery.data ?? []).map((note) => note.name.replace(/\.(md|mb)$/i, '')),
      ...(dailyQuery.data ?? []).map((note) => note.date),
    ]),
  ]

  const items: string[] = (() => {
    if (completion === null) return []
    const query = completion.query.toLowerCase()
    const source = completion.kind === 'tag' ? tagNames : noteNames
    const matches = source.filter((name) => name.toLowerCase().includes(query))
    return matches.sort(
      (a, b) =>
        Number(b.toLowerCase().startsWith(query)) - Number(a.toLowerCase().startsWith(query)) ||
        a.localeCompare(b),
    )
  })()
  const viewport = useSelectionViewport(items.length, MAX_ITEMS)
  const activeIndex = viewport.selection
  const open = completion !== null && items.length > 0

  const handleChange = (value: string) => {
    setText(value)
    const caret = inputRef.current?.selectionStart ?? value.length
    const next = detectCompletion(value, caret)
    setCompletion(next)
    viewport.reset()
  }

  const accept = (item: string) => {
    if (completion === null) return
    const caret = inputRef.current?.selectionStart ?? text.length
    const replacement = completion.kind === 'tag' ? `#${item} ` : `[[${item}]]`
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
    if (body === '' || sending) return
    const submitted = text
    void appendToday
      .mutateAsync(body)
      .then(() => {
        clearSubmittedDraft('daily', submitted)
        setCompletion(null)
      })
      .catch(() => {
        /* The mutation hook reports errors and keeps the draft. */
      })

    inputRef.current?.focus()
  }

  return (
    <div className="relative rounded-md border border-border-strong bg-panel px-2.5 py-1.5 focus-within:border-accent">
      {open && (
        <ul
          id={popupId}
          className="absolute left-0 top-full z-20 mt-1 w-full overflow-hidden rounded-md border border-border-strong bg-overlay py-1 shadow-lg"
          data-testid="completion-popup"
          role="listbox"
          aria-label="Completions"
          data-viewport-start={viewport.offset}
        >
          {items.slice(viewport.offset, viewport.offset + MAX_ITEMS).map((item, row) => {
            const index = viewport.offset + row
            return (
              <li key={item}>
                <button
                  type="button"
                  id={`${popupId}-${index}`}
                  role="option"
                  aria-selected={index === activeIndex}
                  onMouseDown={(event) => {
                    // mousedown: keep textarea focus/selection intact.
                    event.preventDefault()
                    accept(item)
                  }}
                  className={`selection-row flex h-7 w-full items-center gap-2 px-3 py-1 text-left text-[12px] transition-colors ${
                    index === activeIndex
                      ? 'bg-accent/20 text-accent-strong'
                      : 'text-fg hover:bg-raised'
                  }`}
                >
                  <span className="font-mono text-[10px] text-faint">
                    {completion?.kind === 'tag' ? '#' : '[['}
                  </span>
                  <span className="truncate">{item}</span>
                </button>
              </li>
            )
          })}
        </ul>
      )}
      <textarea
        ref={inputRef}
        value={text}
        disabled={sending}
        aria-label="Daily note"
        aria-autocomplete="list"
        aria-controls={open ? popupId : undefined}
        aria-expanded={open}
        aria-activedescendant={open ? `${popupId}-${activeIndex}` : undefined}
        placeholder="Capture a thought, a task, or a link…"
        data-testid="daily-quick-input"
        onChange={(event) => handleChange(event.target.value)}
        onSelect={(event) => {
          const next = detectCompletion(text, event.currentTarget.selectionStart)
          if (
            next?.query !== completion?.query ||
            next?.triggerStart !== completion?.triggerStart ||
            next?.kind !== completion?.kind
          ) {
            setCompletion(next)
            viewport.reset()
          }
        }}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
          if (open) {
            if (event.key === 'ArrowDown') {
              event.preventDefault()
              viewport.move(1)
              return
            }
            if (event.key === 'ArrowUp') {
              event.preventDefault()
              viewport.move(-1)
              return
            }
            if (event.key === 'Tab' || event.key === 'Enter') {
              event.preventDefault()
              const item = items[activeIndex]
              if (item !== undefined) accept(item)
              return
            }
            if (event.key === 'Escape') {
              event.preventDefault()
              setCompletion(null)
              return
            }
          }
          if (event.key === 'Enter' && !event.shiftKey && !event.altKey) {
            event.preventDefault()
            send()
          }
        }}
        onBlur={() => setCompletion(null)}
        className="max-h-48 w-full resize-none overflow-y-auto field-sizing-content bg-transparent text-[13px] text-fg outline-none placeholder:text-faint"
      />
      {appendToday.isError && (
        <p role="alert" className="my-2 text-xs text-danger">
          Could not save. Your draft is kept — try sending again.
        </p>
      )}
      <div className="flex items-center justify-between gap-2">
        <span className="text-[10px] text-faint">Enter to save · Shift+Enter for a new line</span>
        <button
          type="button"
          onClick={send}
          disabled={text.trim() === '' || sending}
          className="inline-flex h-6 items-center gap-1 rounded-sm border border-accent/60 bg-accent/20 px-2 text-[11px] font-medium text-accent-strong transition-colors hover:bg-accent/30 disabled:cursor-not-allowed disabled:opacity-50"
        >
          <SendHorizontal size={11} />
          {sending ? 'Sending…' : 'Send'}
        </button>
      </div>
    </div>
  )
}
