interface IconProps {
  className?: string
}

function Icon({ children, className = 'size-4' }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      viewBox="0 0 20 20"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {children}
    </svg>
  )
}

export const IconDashboard = (props: IconProps) => (
  <Icon {...props}>
    <path d="M3 3h6v6H3zM11 3h6v4h-6zM11 9h6v8h-6zM3 11h6v6H3z" />
  </Icon>
)

export const IconWorkflow = (props: IconProps) => (
  <Icon {...props}>
    <path d="M4 5h4v4H4zM12 11h4v4h-4zM8 7h2.5a1.5 1.5 0 0 1 1.5 1.5V11" />
  </Icon>
)

export const IconJobs = (props: IconProps) => (
  <Icon {...props}>
    <path d="M3 5h14M3 10h14M3 15h9" />
  </Icon>
)

export const IconDevice = (props: IconProps) => (
  <Icon {...props}>
    <rect x="3" y="4" width="14" height="9" rx="1.5" />
    <path d="M7 17h6M10 13v4" />
  </Icon>
)

export const IconKey = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="7" cy="7" r="3" />
    <path d="m9.2 9.2 6 6M13 13l-1.5 1.5M15.2 15.2 14 16.4" />
  </Icon>
)

export const IconMembers = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="8" cy="7" r="2.5" />
    <path d="M3.5 16c0-2.5 2-4 4.5-4s4.5 1.5 4.5 4M14 8.5a2 2 0 1 0 0-4M15 15.5c0-1.8-.8-3-2-3.4" />
  </Icon>
)

export const IconQuota = (props: IconProps) => (
  <Icon {...props}>
    <path d="M10 3a7 7 0 1 0 7 7h-7z" />
    <path d="M10 3v7h7A7 7 0 0 0 10 3z" strokeOpacity="0.4" />
  </Icon>
)

export const IconAudit = (props: IconProps) => (
  <Icon {...props}>
    <path d="M5 3h7l3 3v11H5zM12 3v3h3M7.5 10h5M7.5 13h3" />
  </Icon>
)

export const IconSettings = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="10" cy="10" r="2.5" />
    <path d="M10 3v1.5M10 15.5V17M3 10h1.5M15.5 10H17M5 5l1 1M14 14l1 1M15 5l-1 1M6 14l-1 1" />
  </Icon>
)

export const IconLogout = (props: IconProps) => (
  <Icon {...props}>
    <path d="M8 17H4V3h4M13 6.5 16.5 10 13 13.5M16 10H7.5" />
  </Icon>
)

export const IconChevron = (props: IconProps) => (
  <Icon {...props}>
    <path d="m7 4 5 6-5 6" />
  </Icon>
)

export const IconPlus = (props: IconProps) => (
  <Icon {...props}>
    <path d="M10 4v12M4 10h12" />
  </Icon>
)

export const IconRefresh = (props: IconProps) => (
  <Icon {...props}>
    <path d="M16.5 10a6.5 6.5 0 1 1-2-4.7M16.5 4v3h-3" />
  </Icon>
)

export const IconDownload = (props: IconProps) => (
  <Icon {...props}>
    <path d="M10 3v9M6.5 8.5 10 12l3.5-3.5M4 15.5h12" />
  </Icon>
)

export const IconMenu = (props: IconProps) => (
  <Icon {...props}>
    <path d="M3 5h14M3 10h14M3 15h14" />
  </Icon>
)

export const IconSun = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="10" cy="10" r="3.5" />
    <path d="M10 2.5V4M10 16v1.5M2.5 10H4M16 10h1.5M4.7 4.7l1 1M14.3 14.3l1 1M15.3 4.7l-1 1M5.7 14.3l-1 1" />
  </Icon>
)

export const IconMoon = (props: IconProps) => (
  <Icon {...props}>
    <path d="M15.5 12.3A6.5 6.5 0 0 1 7.7 4.5a6.5 6.5 0 1 0 7.8 7.8z" />
  </Icon>
)

export const IconImage = (props: IconProps) => (
  <Icon {...props}>
    <rect x="3" y="3" width="14" height="14" rx="2" />
    <circle cx="7.5" cy="7.5" r="1.5" />
    <path d="m3 14 4.5-4.5 4 4 2.5-2.5 3 3" />
  </Icon>
)

export const IconVideo = (props: IconProps) => (
  <Icon {...props}>
    <rect x="2" y="4" width="12" height="12" rx="2" />
    <path d="m14 8 4-2.5v9L14 12" />
  </Icon>
)

export const IconAudio = (props: IconProps) => (
  <Icon {...props}>
    <path d="M7 4v12M3 8v4M11 6v8M15 7v6M19 9v2" />
  </Icon>
)

export const IconFile = (props: IconProps) => (
  <Icon {...props}>
    <path d="M4 3h8l4 4v10a1.5 1.5 0 0 1-1.5 1.5h-10A1.5 1.5 0 0 1 4 17V3z" />
    <path d="M12 3v4h4" />
  </Icon>
)

export const IconPlay = (props: IconProps) => (
  <Icon {...props}>
    <path d="m6 4 10 6-10 6V4z" />
  </Icon>
)

export const IconPause = (props: IconProps) => (
  <Icon {...props}>
    <path d="M6 4h3v12H6zM11 4h3v12h-3z" />
  </Icon>
)

export const IconEye = (props: IconProps) => (
  <Icon {...props}>
    <path d="M2 10s3-6 8-6 8 6 8 6-3 6-8 6-8-6-8-6z" />
    <circle cx="10" cy="10" r="2.5" />
  </Icon>
)

export const IconExpand = (props: IconProps) => (
  <Icon {...props}>
    <path d="M13 3h4v4M7 17H3v-4M17 3l-5 5M3 17l5-5" />
  </Icon>
)

export const IconExternalLink = (props: IconProps) => (
  <Icon {...props}>
    <path d="M13 3h4v4M10 10l7-7M15 11v5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 3 16V7a1.5 1.5 0 0 1 1.5-1.5H10" />
  </Icon>
)

export const IconGrid = (props: IconProps) => (
  <Icon {...props}>
    <rect x="3" y="3" width="6" height="6" rx="1" />
    <rect x="11" y="3" width="6" height="6" rx="1" />
    <rect x="3" y="11" width="6" height="6" rx="1" />
    <rect x="11" y="11" width="6" height="6" rx="1" />
  </Icon>
)

export const IconList = (props: IconProps) => (
  <Icon {...props}>
    <path d="M3 5h14M3 10h14M3 15h14" />
  </Icon>
)

export const IconCopy = (props: IconProps) => (
  <Icon {...props}>
    <rect x="7" y="7" width="9" height="9" rx="1.5" />
    <path d="M4 13V5a1.5 1.5 0 0 1 1.5-1.5H12" />
  </Icon>
)

export const IconCheck = (props: IconProps) => (
  <Icon {...props}>
    <path d="m4 10 4 4 8-8" />
  </Icon>
)

export const IconClose = (props: IconProps) => (
  <Icon {...props}>
    <path d="m4 4 12 12M16 4 4 16" />
  </Icon>
)

export const IconZoomIn = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="9" cy="9" r="6" />
    <path d="m14 14 4 4M9 6v6M6 9h6" />
  </Icon>
)

export const IconZoomOut = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="9" cy="9" r="6" />
    <path d="m14 14 4 4M6 9h6" />
  </Icon>
)

export const IconSearch = (props: IconProps) => (
  <Icon {...props}>
    <circle cx="9" cy="9" r="6" />
    <path d="m14 14 4 4" />
  </Icon>
)

export const IconSparkles = (props: IconProps) => (
  <Icon {...props}>
    <path d="m10 2 1.8 4.2L16 8l-4.2 1.8L10 14l-1.8-4.2L4 8l4.2-1.8zM16 12l.9 2.1L19 15l-2.1.9L16 18l-.9-2.1L13 15l2.1-.9z" />
  </Icon>
)

export const IconCompare = (props: IconProps) => (
  <Icon {...props}>
    <path d="M10 3v14M4 7h12M4 13h12" />
  </Icon>
)

export const IconShare = (props: IconProps) => (
  <Icon {...props}>
    <path d="M4 12v5a1.5 1.5 0 0 0 1.5 1.5h9a1.5 1.5 0 0 0 1.5-1.5v-5M13 7l-3-3-3 3M10 4v10" />
  </Icon>
)

export const IconBell = (props: IconProps) => (
  <Icon {...props}>
    <path d="M10 3a4 4 0 0 0-4 4v4l-1.5 2h11L14 11V7a4 4 0 0 0-4-4zM8.5 16a1.5 1.5 0 0 0 3 0" />
  </Icon>
)

export const IconGallery = (props: IconProps) => (
  <Icon {...props}>
    <rect x="3" y="3" width="6" height="5" rx="1" />
    <rect x="11" y="3" width="6" height="7" rx="1" />
    <rect x="3" y="10" width="6" height="7" rx="1" />
    <rect x="11" y="12" width="6" height="5" rx="1" />
  </Icon>
)

export const IconLogo = ({ className = 'size-7' }: IconProps) => (
  <svg viewBox="0 0 32 32" className={className} aria-hidden="true">
    <defs>
      <linearGradient id="nl-logo" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0%" stopColor="var(--app-accent)" />
        <stop offset="100%" stopColor="var(--app-violet)" />
      </linearGradient>
    </defs>
    <rect width="32" height="32" rx="9" fill="url(#nl-logo)" />
    <path
      d="M8 20.5c2.2-3.4 4.1-5.1 5.8-5.1 1.7 0 2.6 1.4 4.2 1.4 1.6 0 3-1.2 6-4.8"
      fill="none"
      stroke="var(--app-accent-fg)"
      strokeWidth="2"
      strokeLinecap="round"
      opacity="0.95"
    />
    <circle cx="11" cy="11" r="2" fill="var(--app-accent-fg)" opacity="0.85" />
  </svg>
)
