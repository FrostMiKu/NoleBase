/**
 * One-shot escape hatch for navigations that have already resolved the
 * dirty conflict themselves (discard, save-and-continue, file-moved
 * resolution). The blocker consults this before showing the dialog.
 */
let permitted = false

export function permitNavigationOnce(): void {
  permitted = true
}

export function consumeNavigationPermission(): boolean {
  const was = permitted
  permitted = false
  return was
}
