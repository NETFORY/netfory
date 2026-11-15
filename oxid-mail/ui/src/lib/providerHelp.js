// Пошаговые инструкции по паролям приложений для провайдеров с 2FA.
// URL показываем копируемым текстом (не <a target=_blank> — в Tauri это не работает).
export const providerHelp = {
  gmail: {
    title: 'Gmail needs an App Password',
    steps: [
      'Google Account → Security → enable 2-Step Verification (you already have it)',
      'Open App passwords and create one for "Mail"',
      'Paste the 16-character code below instead of your password (spaces are OK)',
    ],
    url: 'https://myaccount.google.com/apppasswords',
  },
  yandex: {
    title: 'Yandex needs an app password',
    steps: [
      'Yandex ID → Security → «Пароли приложений» → create one for Mail (IMAP/SMTP)',
      'Also enable IMAP: Yandex.Mail → Settings → «Почтовые программы»',
      'Use that app password below',
    ],
    url: 'https://id.yandex.ru/security/app-passwords',
  },
  yahoo: {
    title: 'Yahoo needs an app password',
    steps: [
      'Yahoo Account → Security → Generate app password',
      'Choose "Other app", name it Oxid Mail',
      'Use the generated code below',
    ],
    url: 'https://login.yahoo.com/account/security',
  },
  custom: null,
}
