package __PACKAGE__

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.webkit.WebView

/**
 * MainActivity NETFORY Messenger (шаблон из messenger/android/, копируется scripts/android-setup.cjs).
 * TauriActivity генерируется tauri в этом же пакете (generated/TauriActivity.kt).
 *  • onStart/onResume → CallService.start: foreground service держит P2P и звонок в фоне;
 *    повторный start после выдачи RECORD_AUDIO переводит сервис в тип phoneCall|microphone;
 *  • один раз просим исключить приложение из оптимизации батареи (Doze режет сеть P2P);
 *  • системная «назад» → JS `window.__ntfryBack()` (закрыть модалку / вернуться к списку чатов);
 *    если JS нечего закрывать — сворачиваем приложение (moveTaskToBack), а не убиваем его;
 *  • extra `ntfry_call` = answer|decline (кнопки уведомления / экран входящего) → JS
 *    `window.__ntfryCallAction` + событие `ntfry-call-action`;
 *  • edge-to-edge (Android 15+): системные отступы (статус-бар, навигация, клавиатура) превращаем
 *    в padding корневого view — иначе строка ввода уходит под навигационную панель, а шапка под статус-бар.
 */
class MainActivity : TauriActivity() {
    private val ui = Handler(Looper.getMainLooper())

    companion object {
        @Volatile
        private var instance: MainActivity? = null

        /** Действие звонка из Telecom/уведомления: в живую Activity напрямую, иначе — через Intent. */
        fun deliverCallAction(ctx: Context, action: String, room: String) {
            val a = instance
            if (a != null && !a.isFinishing) {
                a.runOnUiThread { a.pushCallAction(action, room) }
            } else {
                try {
                    ctx.startActivity(
                        Intent(ctx, MainActivity::class.java)
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                            .putExtra(CallService.EXTRA_ACTION, action)
                            .putExtra(CallService.EXTRA_ROOM, room),
                    )
                } catch (_: Exception) {
                }
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        instance = this
        applySystemInsets()
        requestIgnoreBatteryOptimizationsOnce()
        requestFullScreenIntentOnce()
        handleCallIntent(intent)
    }

    /** Статус-бар/навигация/клавиатура → padding контента; фон под панелями — цвет приложения, иконки светлые. */
    private fun applySystemInsets() {
        val content = findViewById<View>(android.R.id.content) ?: return
        window.decorView.setBackgroundColor(0xFF0B0F14.toInt())
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            content.setOnApplyWindowInsetsListener { v, insets ->
                val bars = insets.getInsets(
                    WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime(),
                )
                v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
                WindowInsets.CONSUMED
            }
            window.insetsController?.setSystemBarsAppearance(
                0,
                WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS,
            )
        } else {
            @Suppress("DEPRECATION")
            content.setOnApplyWindowInsetsListener { v, insets ->
                v.setPadding(
                    insets.systemWindowInsetLeft,
                    insets.systemWindowInsetTop,
                    insets.systemWindowInsetRight,
                    insets.systemWindowInsetBottom,
                )
                insets.consumeSystemWindowInsets()
            }
        }
        content.requestApplyInsets()
    }

    /** Android 14+: полноэкранные уведомления звонков для не-«звонилок» из Play включает пользователь. */
    private fun requestFullScreenIntentOnce() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) return
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        if (nm.canUseFullScreenIntent()) return
        val prefs = getSharedPreferences("ntfry", MODE_PRIVATE)
        if (prefs.getBoolean("fsi_asked", false)) return
        prefs.edit().putBoolean("fsi_asked", true).apply()
        try {
            startActivity(Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, Uri.parse("package:$packageName")))
        } catch (_: Exception) {
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleCallIntent(intent)
    }

    /** WebView создаётся wry асинхронно — ищем по дереву view при каждом обращении. */
    private fun findWebView(v: View? = findViewById(android.R.id.content)): WebView? {
        if (v is WebView) return v
        if (v is ViewGroup) for (i in 0 until v.childCount) findWebView(v.getChildAt(i))?.let { return it }
        return null
    }

    private fun handleCallIntent(intent: Intent?) {
        val action = intent?.getStringExtra(CallService.EXTRA_ACTION) ?: return
        val room = intent.getStringExtra(CallService.EXTRA_ROOM) ?: ""
        intent.removeExtra(CallService.EXTRA_ACTION)
        pushCallAction(action, room)
    }

    /** action: answer | decline | hangup | mute | unmute → JS (ChatWidget consumeCallAction). */
    fun pushCallAction(action: String, room: String) {
        if (action == "answer" || action == "decline") CallService.hideIncoming(this)
        val safeRoom = room.replace("'", "")
        val js = "window.__ntfryCallAction={action:'$action',room:'$safeRoom'};" +
            "window.dispatchEvent(new CustomEvent('ntfry-call-action',{detail:window.__ntfryCallAction}));"
        deliverJs(js, 60)
    }

    /** Выполнить JS, когда WebView уже существует (ретраи по 250 мс, холодный старт из уведомления). */
    private fun deliverJs(js: String, triesLeft: Int) {
        val wv = findWebView()
        if (wv != null) {
            wv.evaluateJavascript(js, null)
        } else if (triesLeft > 0) {
            ui.postDelayed({ deliverJs(js, triesLeft - 1) }, 250)
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        val wv = findWebView()
        if (wv == null) {
            moveTaskToBack(true)
            return
        }
        wv.evaluateJavascript("(function(){try{return !!(window.__ntfryBack&&window.__ntfryBack())}catch(e){return false}})()") { r ->
            if (r != "true") moveTaskToBack(true)
        }
    }

    override fun onStart() {
        super.onStart()
        CallService.start(this)
    }

    override fun onResume() {
        super.onResume()
        CallService.start(this)
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        if (isFinishing) CallService.stop(this)
        super.onDestroy()
    }

    private fun requestIgnoreBatteryOptimizationsOnce() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        if (pm.isIgnoringBatteryOptimizations(packageName)) return
        val prefs = getSharedPreferences("ntfry", MODE_PRIVATE)
        if (prefs.getBoolean("battery_asked", false)) return
        prefs.edit().putBoolean("battery_asked", true).apply()
        try {
            startActivity(
                Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")),
            )
        } catch (_: Exception) {
        }
    }
}
