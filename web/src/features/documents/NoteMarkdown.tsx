import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { toast } from 'sonner'
import { Markdown } from '@/features/agent/Markdown'
import { useDailyNotes, useNotes } from '@/queries/workspace'
import {
  Dialog,
  DialogPortal,
  DialogOverlay,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { resolveNotePath } from './note-links'

export function NoteMarkdown({ text, path }: { text: string; path: string }) {
  const navigate = useNavigate()
  const notes = useNotes()
  const daily = useDailyNotes()
  const [choices, setChoices] = useState<string[]>([])
  const open = (target: string) => {
    setChoices([])
    void navigate({ to: '/document', search: { path: target } })
  }
  const follow = (href: string) => {
    if (href.startsWith('/nole-wiki?')) {
      const target = new URLSearchParams(href.split('?')[1]).get('target') ?? ''
      const stem = (name: string) => name.replace(/\.(md|mb)$/i, '').toLowerCase()
      const matches = [
        ...(notes.data ?? []).map((note) => note.path),
        ...(daily.data ?? []).map((note) => note.path),
      ].filter(
        (candidate) =>
          candidate === target || stem(candidate.split('/').pop() ?? '') === stem(target),
      )
      if (matches.length === 1) open(matches[0]!)
      else if (matches.length > 1) setChoices(matches)
      else toast.info(`No note named “${target}”. Create it with New note.`)
      return true
    }
    if (href.startsWith('/tag/')) {
      void navigate({ to: '/tag/$tag', params: { tag: decodeURIComponent(href.slice(5)) } })
      return true
    }
    const resolved = resolveNotePath(href, path)
    if (resolved) {
      open(resolved)
      return true
    }
    return false
  }
  return (
    <>
      <Markdown text={text} noteLinks onFollowLink={follow} />
      <Dialog
        open={choices.length > 0}
        onOpenChange={(open) => {
          if (!open) setChoices([])
        }}
      >
        <DialogPortal>
          <DialogOverlay />
          <DialogContent>
            <DialogTitle>Choose a note</DialogTitle>
            <DialogDescription>Several notes match this link.</DialogDescription>
            <div className="mt-4 space-y-1">
              {choices.map((choice) => (
                <button
                  key={choice}
                  type="button"
                  onClick={() => open(choice)}
                  className="block w-full rounded p-2 text-left text-xs hover:bg-raised"
                >
                  {choice}
                </button>
              ))}
            </div>
          </DialogContent>
        </DialogPortal>
      </Dialog>
    </>
  )
}
