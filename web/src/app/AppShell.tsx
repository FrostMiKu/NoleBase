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
import { ActivityRail } from '@/components/ActivityRail'
import { SplitGroup, SplitPanel, SplitSeparator } from '@/components/SplitPane'
import { StatusBar } from '@/components/StatusBar'
import { TopBar } from '@/components/TopBar'
import { AgentPanel } from '@/features/agent/AgentPanel'
import { AgentStatusPanel } from '@/features/agent/AgentStatusPanel'
import { LeftSidebar } from '@/features/navigation/LeftSidebar'
import { TodoPanel } from '@/features/todos/TodoPanel'
import { useUiStore } from '@/stores/ui-store'
import { useMediaQuery } from '@/hooks/use-media-query'
import { CreateNoteDialog } from '@/features/documents/CreateNoteDialog'
import {
  Dialog,
  DialogPortal,
  DialogOverlay,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { queryClient } from '@/queries/query-client'
import { workspaceKeys } from '@/queries/keys'

/** The document path currently selected in the router, if any. */
function useCurrentDocumentPath(): string | null {
  const location = useRouterState({ select: (state) => state.location })
  if (location.pathname !== '/document') return null
  const search = location.search as { path?: unknown }
  return typeof search?.path === 'string' ? search.path : null
}

/** Left rail + sidebar + main + right sidebar (Todo / Agent) desktop layout. */
export function AppShell() {
  const navigate = useNavigate()
  const currentPath = useCurrentDocumentPath()
  const createOpen = useUiStore((state) => state.createNoteOpen)
  const location = useRouterState({ select: (state) => state.location.href })
  useEffect(() => {
    useUiStore.getState().setMobilePanel(null)
  }, [location])

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
      <div className="flex min-h-0 flex-1">
        <ActivityRail />
        <main className="min-w-0 flex-1">
          <MainPanels />
        </main>
      </div>
      <StatusBar />
      <CommandPalette />
      <DirtyGuard />
      <FileMovedConflict />
      {createOpen && (
        <CreateNoteDialog onClose={() => useUiStore.getState().setCreateNoteOpen(false)} />
      )}
    </div>
  )
}

function MainPanels() {
  const compact = useMediaQuery('(max-width: 1023px)')
  const mobilePanel = useUiStore((state) => state.mobilePanel)
  const leftSidebarOpen = useUiStore((state) => state.leftSidebarOpen)
  const rightSidebarOpen = useUiStore((state) => state.rightSidebarOpen)
  const leftSidebarSize = useUiStore((state) => state.leftSidebarSize)
  const rightSidebarSize = useUiStore((state) => state.rightSidebarSize)

  return (
    <>
      <SplitGroup orientation="horizontal" className="h-full">
        {!compact && leftSidebarOpen && (
          <>
            <SplitPanel
              id="left-sidebar"
              defaultSize={`${leftSidebarSize}%`}
              minSize="14%"
              maxSize="40%"
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
        {!compact && rightSidebarOpen && (
          <>
            <SplitSeparator />
            <SplitPanel
              id="right-sidebar"
              defaultSize={`${rightSidebarSize}%`}
              minSize="18%"
              maxSize="45%"
              className="border-l border-border bg-panel"
              onResize={(size) => useUiStore.getState().setRightSidebarSize(size.asPercentage)}
            >
              <RightSidebar />
            </SplitPanel>
          </>
        )}
      </SplitGroup>
      <Dialog
        open={compact && mobilePanel !== null}
        onOpenChange={(open) => {
          if (!open) useUiStore.getState().setMobilePanel(null)
        }}
      >
        <DialogPortal>
          <DialogOverlay />
          <DialogContent size="drawer" padded={false} className="flex h-[85dvh] flex-col">
            <DialogTitle className="flex h-10 shrink-0 items-center border-b border-border px-3 pr-10">
              {mobilePanel === 'files' ? 'Files' : 'Tasks and Agent'}
            </DialogTitle>
            <DialogDescription className="sr-only">
              {mobilePanel === 'files'
                ? 'Browse and organize your notes.'
                : 'Track tasks and work with the Agent.'}
            </DialogDescription>
            <div className="min-h-0 flex-1">
              {mobilePanel === 'files' ? <LeftSidebar /> : <RightSidebar />}
            </div>
          </DialogContent>
        </DialogPortal>
      </Dialog>
    </>
  )
}

/** Right sidebar: Todos on top and, below, the compact agent surface —
 * the mini composer/transcript everywhere except the /agent center view,
 * where the transcript already fills the center and the slot shows
 * background jobs and subagent runs instead. Vertically resizable. */
function RightSidebar() {
  const todoPanelSize = useUiStore((state) => state.todoPanelSize)
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const agentCentered = pathname === '/agent'

  return (
    <SplitGroup orientation="vertical" className="h-full">
      <SplitPanel
        id="todos-slot"
        defaultSize={`${todoPanelSize}%`}
        minSize="15%"
        maxSize="80%"
        className="border-b border-border"
        onResize={(size) => useUiStore.getState().setTodoPanelSize(size.asPercentage)}
      >
        <TodoPanel />
      </SplitPanel>
      <SplitSeparator vertical />
      <SplitPanel id="agent-slot" minSize="15%">
        {agentCentered ? <AgentStatusPanel /> : <AgentPanel />}
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
