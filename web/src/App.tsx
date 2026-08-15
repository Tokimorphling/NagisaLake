import { lazy, Suspense } from 'react'
import { Navigate, Route, Routes } from 'react-router-dom'
import { useAuth } from '@/state/auth'
import { AppLayout } from '@/components/layout/AppLayout'
import { ErrorBoundary } from '@/components/ui/ErrorBoundary'
import { IconLogo } from '@/components/layout/icons'
import { DashboardPage } from '@/pages/DashboardPage'
import { AuthPage } from '@/pages/AuthPage'
import { NotificationProvider } from '@/state/notifications'

// Dashboard is the authenticated landing route and stays eager so the first
// paint after login is immediate; the rest are split into on-demand chunks.
const WorkflowsPage = lazy(() => import('@/pages/workflows/WorkflowsPage').then((m) => ({ default: m.WorkflowsPage })))
const WorkflowDetailPage = lazy(() =>
  import('@/pages/workflows/WorkflowDetailPage').then((m) => ({ default: m.WorkflowDetailPage })),
)
const JobsPage = lazy(() => import('@/pages/jobs/JobsPage').then((m) => ({ default: m.JobsPage })))
const JobDetailPage = lazy(() => import('@/pages/jobs/JobDetailPage').then((m) => ({ default: m.JobDetailPage })))
const BatchesPage = lazy(() => import('@/pages/batches/BatchesPage').then((m) => ({ default: m.BatchesPage })))
const BatchDetailPage = lazy(() =>
  import('@/pages/batches/BatchDetailPage').then((m) => ({ default: m.BatchDetailPage })),
)
const DevicesPage = lazy(() => import('@/pages/DevicesPage').then((m) => ({ default: m.DevicesPage })))
const CredentialsPage = lazy(() => import('@/pages/CredentialsPage').then((m) => ({ default: m.CredentialsPage })))
const MembersPage = lazy(() => import('@/pages/MembersPage').then((m) => ({ default: m.MembersPage })))
const QuotaPage = lazy(() => import('@/pages/QuotaPage').then((m) => ({ default: m.QuotaPage })))
const AuditPage = lazy(() => import('@/pages/AuditPage').then((m) => ({ default: m.AuditPage })))
const SettingsPage = lazy(() => import('@/pages/SettingsPage').then((m) => ({ default: m.SettingsPage })))
const GalleryPage = lazy(() => import('@/pages/GalleryPage').then((m) => ({ default: m.GalleryPage })))

function RouteFallback() {
  return (
    <div className="grid min-h-[50vh] place-items-center">
      <div className="flex flex-col items-center gap-3 text-subtle">
        <IconLogo className="size-8 animate-pulse" />
        <p className="text-xs">加载中…</p>
      </div>
    </div>
  )
}

export function App() {
  const { status } = useAuth()

  // Session restore runs before the first paint decision so a reload does not
  // flash the login screen for an already-authenticated user.
  if (status === 'loading') {
    return (
      <div className="aurora grid min-h-dvh place-items-center">
        <div className="flex flex-col items-center gap-3">
          <IconLogo className="size-10 animate-pulse" />
          <p className="text-xs text-muted">正在恢复会话…</p>
        </div>
      </div>
    )
  }

  if (status === 'anonymous') {
    return (
      <Routes>
        <Route path="*" element={<AuthPage />} />
      </Routes>
    )
  }

  // NotificationProvider polls the job list and needs both the router (to focus a
  // job from a notification click) and an authenticated session, so it wraps the
  // routes rather than sitting in main.tsx.
  return (
    <NotificationProvider>
      <ErrorBoundary>
        <Routes>
          <Route element={<AppLayout />}>
            <Route path="/" element={<DashboardPage />} />
            <Route
              path="/workflows"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <WorkflowsPage />
                </Suspense>
              }
            />
            <Route
              path="/workflows/:workflowId"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <WorkflowDetailPage />
                </Suspense>
              }
            />
            <Route
              path="/jobs"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <JobsPage />
                </Suspense>
              }
            />
            <Route
              path="/jobs/:jobId"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <JobDetailPage />
                </Suspense>
              }
            />
            <Route
              path="/batches"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <BatchesPage />
                </Suspense>
              }
            />
            <Route
              path="/batches/:batchId"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <BatchDetailPage />
                </Suspense>
              }
            />
            <Route
              path="/devices"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <DevicesPage />
                </Suspense>
              }
            />
            <Route
              path="/credentials"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <CredentialsPage />
                </Suspense>
              }
            />
            <Route
              path="/members"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <MembersPage />
                </Suspense>
              }
            />
            <Route
              path="/quota"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <QuotaPage />
                </Suspense>
              }
            />
            <Route
              path="/audit"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <AuditPage />
                </Suspense>
              }
            />
            <Route
              path="/settings"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <SettingsPage />
                </Suspense>
              }
            />
            <Route
              path="/gallery"
              element={
                <Suspense fallback={<RouteFallback />}>
                  <GalleryPage />
                </Suspense>
              }
            />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </ErrorBoundary>
    </NotificationProvider>
  )
}
