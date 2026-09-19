import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

import { Providers } from '@/app/providers'
import '@/styles/globals.css'

const container = document.getElementById('root')
if (container === null) {
  throw new Error('missing #root element')
}

createRoot(container).render(
  <StrictMode>
    <Providers />
  </StrictMode>,
)
