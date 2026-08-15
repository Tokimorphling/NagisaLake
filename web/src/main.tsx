import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ApiError } from '@/api/client'
import { App } from './App'
import { AuthProvider } from '@/state/auth'
import { ThemeProvider } from '@/state/theme'
import { ToastProvider } from '@/state/toast'
import { Toaster } from '@/components/ui/Toaster'
import './styles.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 10_000,
      refetchOnWindowFocus: false,
      // 401 is handled by the client's refresh path; 403/404 will not change on retry.
      retry: (failureCount, error) =>
        error instanceof ApiError && error.status >= 500 ? failureCount < 2 : false,
    },
  },
})

const container = document.getElementById('root')
if (!container) throw new Error('#root container is missing from index.html')

createRoot(container).render(
  <StrictMode>
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <ToastProvider>
          <AuthProvider>
            <BrowserRouter>
              <App />
            </BrowserRouter>
            <Toaster />
          </AuthProvider>
        </ToastProvider>
      </QueryClientProvider>
    </ThemeProvider>
  </StrictMode>,
)
