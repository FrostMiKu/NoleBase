import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { workspaceApi } from '@/api/workspace'
import { workspaceKeys } from './keys'

export function useWorkspaceOverview() {
  return useQuery({
    queryKey: workspaceKeys.summary(),
    queryFn: workspaceApi.overview,
  })
}

export function useNotes() {
  return useQuery({ queryKey: workspaceKeys.notes(), queryFn: workspaceApi.notes })
}

export function useDailyNotes() {
  return useQuery({ queryKey: workspaceKeys.daily(), queryFn: workspaceApi.daily })
}

export function useTodos() {
  return useQuery({ queryKey: workspaceKeys.todos(), queryFn: workspaceApi.todos })
}

export function useTags() {
  return useQuery({ queryKey: workspaceKeys.tags(), queryFn: workspaceApi.tags })
}

export function useTagDocuments(tag: string) {
  return useQuery({
    queryKey: workspaceKeys.tag(tag),
    queryFn: () => workspaceApi.tagDocuments(tag),
    enabled: tag.trim() !== '',
  })
}

export function useSearchResults(query: string) {
  return useQuery({
    queryKey: workspaceKeys.search(query),
    queryFn: () => workspaceApi.search(query),
    enabled: query.trim() !== '',
  })
}

export function useAppendToday() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (body: string) => workspaceApi.appendToday(body),
    onSuccess: (note) => {
      // The append may add tasks and always changes the daily set.
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.daily() })
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() })
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() })
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.document(note.path) })
      toast.success(`Saved to ${note.date}`)
    },
    onError: (error) => {
      toast.error(`Could not save: ${error instanceof Error ? error.message : 'unknown error'}`)
    },
  })
}

export function useToggleTodo() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (index: number) => workspaceApi.toggleTodo(index),
    onSuccess: (fresh) => {
      // The response is the refreshed list; the toggle also rewrote a
      // daily file, so the daily river must refetch too.
      queryClient.setQueryData(workspaceKeys.todos(), fresh)
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.daily() })
    },
    onError: (error) => {
      toast.error(`Could not toggle task: ${error instanceof Error ? error.message : 'unknown error'}`)
      // Indices are positional; resync the list in case it shifted.
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() })
    },
  })
}

export function useDocument(path: string) {
  return useQuery({
    queryKey: workspaceKeys.document(path),
    queryFn: () => workspaceApi.document(path),
    enabled: path.trim() !== '',
  })
}

export function useBacklinks(path: string) {
  return useQuery({
    queryKey: workspaceKeys.backlinks(path),
    queryFn: () => workspaceApi.backlinks(path),
    enabled: path.trim() !== '',
  })
}
