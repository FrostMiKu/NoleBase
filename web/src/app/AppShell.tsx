import { useEffect } from 'react'
import { Outlet, useNavigate, useRouterState } from '@tanstack/react-router'

import { setCurrentDocumentPath } from '@/hooks/use-workspace-events'
import { useAgentEvents } from '@/hooks/use-agent-events'
import { useWorkspaceEvents } from '@/hooks/use-workspace-events'
import { useKeyboardShortcuts } from '@/hooks/use-keyboard-shortcuts'
import { DirtyGuard } from '@/editor/DirtyGuard'
import { FileMovedConflict } from '@/editor/FileMovedConflict'
import { editorStore } from '@/editor/editor-state'
import { permitNavigationOnce } from '@/editor/navigation-guard'
import { CommandPalette } from '@/components/CommandPalette'
import { SplitGroup, SplitPanel, SplitSeparator } from '@/components/SplitPane'
import { StatusBar } from '@/components/StatusBar'
import { TopBar } from '@/components/TopBar'
import { AgentPanel } from '@/features/agent/AgentPanel'
import { LeftSidebar } from '@/features/navigation/LeftSidebar'
import { TodoPanel } from '@/features/todos/TodoPanel'
import { useUiStore } from '@/stores/ui-store'
import { queryClient } from '@/queries/query-client'
import { workspaceKeys } from '@/queries/keys'

/** The document path currently selected in the router, if any. */
function useCurrentDocumentPath(): string | null {
  const location = useRouterState({ select: (state) => state.location })
  if (location.pathname !== '/document') return null
  const search = location.search as { path?: unknown }
  return typeof search?.path === 'string' ? search.path : null
}

/** Left sidebar + main + right sidebar (Todo / Agent) desktop layout. */
export function AppShell() {
  const navigate = useNavigate()
  const currentPath = useCurrentDocumentPath()

  useWorkspaceEvents()
  useKeyboardShortcuts()
  useAgentEvents({
    onOpenFile: (path) => {
      // Normal navigation: the dirty guard still protects unsaved edits.
      void navigate({ to: '/document', search: { path } })
    },
    onFileMoved: (from, to) => {
      handleFileMoved(from, to, () => {
        void navigate({ to: '/document', search: { path: to } })
      })
    },
  })

  // Keep the WS layer's view of the open document in sync with the router.
  useEffect(() => {
    setCurrentDocumentPath(currentPath)
  }, [currentPath])

  return (
    <div className="flex h-full flex-col">
      <TopBar />
      <main className="min-h-0 flex-1">
        <MainPanels />
      </main>
      <StatusBar />
      <CommandPalette />
      <DirtyGuard />
      <FileMovedConflict />
    </div>
  )
}

function MainPanels() {
  const leftSidebarOpen = useUiStore((state) => state.leftSidebarOpen)
  const rightSidebarOpen = useUiStore((state) => state.rightSidebarOpen)
  const leftSidebarSize = useUiStore((state) => state.leftSidebarSize)
  const rightSidebarSize = useUiStore((state) => state.rightSidebarSize)

  return (
    <SplitGroup orientation="horizontal" className="h-full">
      {leftSidebarOpen && (
        <>
          <SplitPanel
            id="left-sidebar"
            defaultSize={`${leftSidebarSize}`}
            minSize="14"
            maxSize="40"
            className="border-r border-border bg-panel"
            onResize={(size) => useUiStore.getState().setLeftSidebarSize(size.asPercentage)}
          >
            <LeftSidebar />
          </SplitPanel>
          <SplitSeparator />
        </>
      )}
      <SplitPanel id="main" className="bg-bg">
        <Outlet />
      </SplitPanel>
      {rightSidebarOpen && (
        <>
          <SplitSeparator />
          <SplitPanel
            id="right-sidebar"
            defaultSize={`${rightSidebarSize}`}
            minSize="18"
            maxSize="45"
            className="border-l border-border bg-panel"
            onResize={(size) => useUiStore.getState().setRightSidebarSize(size.asPercentage)}
          >
            <RightSidebar />
          </SplitPanel>
        </>
      )}
    </SplitGroup>
  )
}

/** Right sidebar: Todo on top, Agent below, vertically resizable. */
function RightSidebar() {
  const agentPanelSize = useUiStore((state) => state.agentPanelSize)
  return (
    <SplitGroup orientation="vertical" className="h-full">
      <SplitPanel
        id="todo-panel"
        defaultSize={`${100 - agentPanelSize}`}
        minSize="12"
        maxSize="70"
        className="border-b border-border"
        onResize={(size) => useUiStore.getState().setAgentPanelSize(100 - size.asPercentage)}
      >
        <TodoPanel />
      </SplitPanel>
      <SplitSeparator vertical />
      <SplitPanel id="agent-panel" minSize="20">
        <AgentPanel />
      </SplitPanel>
    </SplitGroup>
  )
}

/**
 * `file_moved` from the agent stream. For the open document: clean
 * editors follow the move; dirty editors must never write to the old
 * path, so the conflict flow takes over. Otherwise just refresh lists.
 */
function handleFileMoved(from: string, to: string, navigateTo: (path: string) => void): void {
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.summary() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.notes() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.tags() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.todos() })
  void queryClient.invalidateQueries({ queryKey: workspaceKeys.searchFamily() })

  const state = editorStore.getState()
  if (state.path !== from) return
  if (state.dirty) {
    editorStore.markMoved(to)
    return
  }
  permitNavigationOnce()
  navigateTo(to)
}
