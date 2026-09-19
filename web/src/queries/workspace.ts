import { useQuery } from '@tanstack/react-query'

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
