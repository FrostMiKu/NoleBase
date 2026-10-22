import '@testing-library/jest-dom/vitest'

// jsdom has no viewport; router scroll restoration is a browser concern.
window.scrollTo = () => {}
