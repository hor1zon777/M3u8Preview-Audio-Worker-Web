/** @type {import('tailwindcss').Config} */
// 沿用 m3u8-preview-go 的 emby 色板，保持视觉风格统一。
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        emby: {
          green: '#52B54B',
          'green-dark': '#3F9039',
          'bg-base': '#0F1115',
          'bg-card': '#181B22',
          'bg-elevated': '#1F232C',
          'bg-input': '#0E1014',
          'bg-dialog': '#161922',
          border: '#2A2F3A',
          'text-primary': '#E5E7EB',
          'text-secondary': '#9CA3AF',
          'text-muted': '#6B7280',
        },
      },
      fontFamily: {
        sans: ['Inter', 'system-ui', '-apple-system', 'Segoe UI', 'Roboto', 'sans-serif'],
        mono: ['JetBrains Mono', 'Consolas', 'Menlo', 'monospace'],
      },
    },
  },
  plugins: [],
};
