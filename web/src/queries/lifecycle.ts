import { useMutation, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { toast } from 'sonner'

import { workspaceApi } from '@/api/workspace'
import { editorStore } from '@/editor/editor-state'
import {
  cancelEditsForLifecycleDeletion,
  flushBeforeLifecycleMutation,
} from '@/editor/lifecycle'
import { permitNavigationOnce } from '@/editor/navigation-guard'
import type { DailyNote } from '@/types/workspace'
import { workspaceKeys } from './keys'

/**
 * Document lifecycle mutations — the web counterpart of the TUI's file
 * actions. Each hook owns the full side-effect sequence: autosave
 * coordination for the open document (§ editor/lifecycle.ts), cache
 * invalidation, and the navigation policy (push for create, replace for
 * retargeting mutations so Back never returns to a dead path).
 *
 * Dialog-driven mutations (create, rename, delete, daily) surface errors in
 * their dialog; the direct context-menu mutations (archive, restore) toast.
 */

async function invalidateNotesLists(queryClient: QueryClient): Promise<void> {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: workspaceKeys.notes() }),
    queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() }),
  ])
}

/** Content-addressed caches a rename/archive/rewrite can shift. */
function invalidateContentIndexes(queryClient: QueryClient): void {
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.searchFamily() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.tagFamily() })
}

/** Drop the old identity's caches and mark the new identity stale. */
async function retargetDocumentCaches(
  queryClient: QueryClient,
  oldPath: string,
  newPath: string,
): Promise<void> {
  queryClient.removeQueries({ queryKey: workspaceKeys.document(oldPath) })
  queryClient.removeQueries({ queryKey: workspaceKeys.backlinks(oldPath) })
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: workspaceKeys.document(newPath) }),
    queryClient.invalidateQueries({ queryKey: workspaceKeys.backlinks(newPath) }),
  ])
}

export interface CreateNoteInput {
  name: string
  fromTemplate: boolean
}

export function useCreateNote() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: (input: CreateNoteInput) =>
      workspaceApi.createNote(input.name, input.fromTemplate),
    onSuccess: async (result) => {
      await invalidateNotesLists(queryClient)
      // Push: a brand-new note is a new history entry. The path comes from
      // the backend response — the client never guesses it.
      void navigate({ to: '/document', search: { path: result.path } })
    },
  })
}

export interface RenameDocumentInput {
  path: string
  newName: string
}

export function useRenameDocument() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: async (input: RenameDocumentInput) => {
      if (editorStore.getState().path === input.path) {
        const flushed = await flushBeforeLifecycleMutation(input.path)
        if (!flushed) {
          throw new Error(
            'Could not save your pending edits, so the note was not renamed. Try saving first.',
          )
        }
      }
      return workspaceApi.renameDocument(input.path, input.newName)
    },
    onSuccess: async (result, input) => {
      await invalidateNotesLists(queryClient)
      invalidateContentIndexes(queryClient)
      const wasCurrent = editorStore.getState().path === input.path
      await retargetDocumentCaches(queryClient, input.path, result.path)
      if (wasCurrent) {
        // Replace, not push: Back must not resurrect the old path.
        void navigate({ to: '/document', search: { path: result.path }, replace: true })
      }
    },
    onError: (_error, input) => {
      // The open document (if any) keeps its path; autosave resumes. The
      // rename dialog displays this error inline (it stays open).
      editorStore.releaseAutosaveBlock(input.path)
    },
  })
}

/** Shared sequence for archive/restore: flush → mutate → retarget. */
function useRetargetingPathMutation(
  mutate: (path: string) => Promise<{ path: string }>,
  label: string,
) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: async (path: string) => {
      if (editorStore.getState().path === path) {
        const flushed = await flushBeforeLifecycleMutation(path)
        if (!flushed) {
          throw new Error(
            `Could not save your pending edits, so the note was not ${label.toLowerCase()}. Try saving first.`,
          )
        }
      }
      return mutate(path)
    },
    onSuccess: async (result, path) => {
      await invalidateNotesLists(queryClient)
      const wasCurrent = editorStore.getState().path === path
      await retargetDocumentCaches(queryClient, path, result.path)
      if (wasCurrent) {
        // The user stays on the same text, now filed under the new path.
        void navigate({ to: '/document', search: { path: result.path }, replace: true })
      }
    },
    onError: (error, path) => {
      editorStore.releaseAutosaveBlock(path)
      toast.error(
        `${label} failed: ${error instanceof Error ? error.message : 'unknown error'}`,
      )
    },
  })
}

export function useArchiveDocument() {
  return useRetargetingPathMutation(workspaceApi.archiveDocument, 'Archive')
}

export function useRestoreDocument() {
  return useRetargetingPathMutation(workspaceApi.restoreDocument, 'Restore')
}

export function useDeleteDocument() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: async (path: string) => {
      if (editorStore.getState().path === path) {
        // Confirmed permanent deletion: pending edits are cancelled, not
        // flushed — the content is about to disappear with the file.
        cancelEditsForLifecycleDeletion(path)
      }
      return workspaceApi.deleteDocument(path)
    },
    onSuccess: async (_result, path) => {
      queryClient.removeQueries({ queryKey: workspaceKeys.document(path) })
      queryClient.removeQueries({ queryKey: workspaceKeys.backlinks(path) })
      await invalidateNotesLists(queryClient)
      invalidateContentIndexes(queryClient)
      if (editorStore.getState().path === path) {
        permitNavigationOnce()
        void navigate({ to: '/', replace: true })
      }
    },
    onError: (_error, path) => {
      // Deletion failed: keep the editor exactly as it was and let autosave
      // run again; the local text was never touched. The confirmation
      // dialog displays this error inline (it stays open).
      editorStore.releaseAutosaveBlock(path)
    },
  })
}

export function useDeleteDaily() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: async (note: DailyNote) => {
      if (editorStore.getState().path === note.path) {
        cancelEditsForLifecycleDeletion(note.path)
      }
      return workspaceApi.deleteDaily(note.date)
    },
    onSuccess: async (_result, note) => {
      queryClient.removeQueries({ queryKey: workspaceKeys.document(note.path) })
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: workspaceKeys.daily() }),
        queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() }),
        // The day's tasks vanish with the file.
        queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() }),
      ])
      if (editorStore.getState().path === note.path) {
        permitNavigationOnce()
        void navigate({ to: '/', replace: true })
      }
    },
    onError: (_error, note) => {
      editorStore.releaseAutosaveBlock(note.path)
    },
  })
}

export interface MoveDailyInput {
  note: DailyNote
  targetPath: string
}

export function useMoveDaily() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: async (input: MoveDailyInput) => {
      if (editorStore.getState().path === input.note.path) {
        const flushed = await flushBeforeLifecycleMutation(input.note.path)
        if (!flushed) {
          throw new Error(
            'Could not save your pending edits, so the daily note was not moved. Try saving first.',
          )
        }
      }
      return workspaceApi.moveDaily(input.note.date, input.targetPath)
    },
    onSuccess: async (result, input) => {
      queryClient.removeQueries({ queryKey: workspaceKeys.document(input.note.path) })
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: workspaceKeys.daily() }),
        queryClient.invalidateQueries({ queryKey: workspaceKeys.notes() }),
        queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() }),
        queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() }),
        queryClient.invalidateQueries({ queryKey: workspaceKeys.tags() }),
      ])
      invalidateContentIndexes(queryClient)
      await queryClient.invalidateQueries({ queryKey: workspaceKeys.document(result.path) })
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.backlinks(result.path) })
      // "Move into this note" always lands on the target, current or not.
      void navigate({ to: '/document', search: { path: result.path }, replace: true })
    },
    onError: (_error, input) => {
      editorStore.releaseAutosaveBlock(input.note.path)
    },
  })
}
