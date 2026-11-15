/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: ['./index.html', './src/**/*.{vue,js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        rust: {
          DEFAULT: '#E25822',
          50: '#FCE9DF',
          100: '#F9D2BD',
          200: '#F2A57C',
          300: '#EB783A',
          400: '#E25822',
          500: '#B8431A',
          600: '#8E3414',
          700: '#65250E',
        },
        iron: {
          DEFAULT: '#2B2D31',
          50: '#E8E9EA',
          100: '#C8CACE',
          200: '#9498A0',
          300: '#5F6571',
          400: '#3A3D44',
          500: '#2B2D31',
          600: '#1F2125',
          700: '#15171A',
          800: '#0E1012',
        },
        copper: {
          DEFAULT: '#4DB6AC',
          50: '#E3F4F2',
          100: '#BFE6E1',
          200: '#8FD3CC',
          300: '#4DB6AC',
          400: '#349990',
          500: '#26756E',
        },
      },
      fontFamily: {
        mono: ['"JetBrains Mono"', '"Roboto Mono"', 'ui-monospace', 'monospace'],
        sans: ['Inter', 'system-ui', '-apple-system', 'sans-serif'],
      },
      boxShadow: {
        'rust-glow': '0 0 24px -4px rgba(226, 88, 34, 0.45)',
        'copper-glow': '0 0 18px -4px rgba(77, 182, 172, 0.5)',
        'panel': '0 1px 0 0 rgba(255,255,255,0.04) inset, 0 0 0 1px rgba(255,255,255,0.04)',
      },
      backgroundImage: {
        'grain': "url(\"data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/%3E%3CfeColorMatrix values='0 0 0 0 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0.06 0'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E\")",
      },
    },
  },
  plugins: [],
}
