import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { refresh, session } from '@/api/client'
import { endpoints } from '@/api/endpoints'
import type { Membership, PublicUser, Role } from '@/api/types'
import { ROLE_RANK } from '@/api/types'

interface AuthState {
  status: 'loading' | 'authenticated' | 'anonymous'
  user: PublicUser | null
  memberships: Membership[]
  organizationId: string | null
}

interface AuthContextValue extends AuthState {
  currentMembership: Membership | null
  role: Role | null
  login: (email: string, password: string) => Promise<void>
  register: (email: string, password: string, organizationName?: string) => Promise<void>
  logout: () => Promise<void>
  switchOrganization: (organizationId: string) => Promise<void>
  reloadMemberships: () => Promise<void>
  /** True when the current role meets or exceeds the required rank. */
  atLeast: (required: Role) => boolean
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient()
  const [state, setState] = useState<AuthState>({
    status: 'loading',
    user: null,
    memberships: [],
    organizationId: null,
  })

  const applyMe = useCallback(async () => {
    const me = await endpoints.me()
    session.setOrganization(me.current_organization_id)
    setState({
      status: 'authenticated',
      user: me.user,
      memberships: me.memberships,
      organizationId: me.current_organization_id,
    })
  }, [])

  const goAnonymous = useCallback(() => {
    session.clear()
    queryClient.clear()
    setState({ status: 'anonymous', user: null, memberships: [], organizationId: null })
  }, [queryClient])

  // Boot: the access token is memory-only, so a reload restores the session
  // from the HttpOnly refresh cookie before deciding to show the login page.
  useEffect(() => {
    let cancelled = false
    void (async () => {
      try {
        await refresh()
        if (cancelled) return
        await applyMe()
      } catch {
        if (!cancelled) {
          setState({ status: 'anonymous', user: null, memberships: [], organizationId: null })
        }
      }
    })()
    return () => {
      cancelled = true
    }
  }, [applyMe])

  // A failed mid-session refresh drops straight to the login page.
  useEffect(() => {
    session.onLost(() => goAnonymous())
    return () => session.onLost(null)
  }, [goAnonymous])

  const login = useCallback(
    async (email: string, password: string) => {
      const body = await endpoints.login({ email, password })
      session.setAuth(body)
      await applyMe()
    },
    [applyMe],
  )

  const register = useCallback(
    async (email: string, password: string, organizationName?: string) => {
      const body = await endpoints.register({
        email,
        password,
        organization_name: organizationName?.trim() ? organizationName.trim() : undefined,
      })
      session.setAuth(body)
      await applyMe()
    },
    [applyMe],
  )

  const logout = useCallback(async () => {
    try {
      await endpoints.logout()
    } catch {
      // Even if revocation fails, drop local state so the UI cannot keep using it.
    }
    goAnonymous()
  }, [goAnonymous])

  // Switching org only changes the in-memory header. API keys stay bound to
  // their own organization and are unaffected.
  const switchOrganization = useCallback(
    async (organizationId: string) => {
      session.setOrganization(organizationId)
      setState((previous) => ({ ...previous, organizationId }))
      queryClient.clear()
      await applyMe()
    },
    [applyMe, queryClient],
  )

  const reloadMemberships = useCallback(async () => {
    const memberships = await endpoints.organizations()
    setState((previous) => ({ ...previous, memberships }))
  }, [])

  const value = useMemo<AuthContextValue>(() => {
    const currentMembership =
      state.memberships.find((m) => m.organization_id === state.organizationId) ?? null
    const role = currentMembership?.role ?? null
    return {
      ...state,
      currentMembership,
      role,
      login,
      register,
      logout,
      switchOrganization,
      reloadMemberships,
      atLeast: (required) => (role ? ROLE_RANK[role] >= ROLE_RANK[required] : false),
    }
  }, [login, logout, register, reloadMemberships, state, switchOrganization])

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth(): AuthContextValue {
  const value = useContext(AuthContext)
  if (!value) throw new Error('useAuth must be used inside AuthProvider')
  return value
}
