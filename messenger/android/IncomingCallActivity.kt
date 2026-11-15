package __PACKAGE__

import android.app.Activity
import android.app.KeyguardManager
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Полноэкранный экран входящего звонка (fullScreenIntent уведомления): показывается поверх
 * заблокированного экрана и включает дисплей. «Ответить»/«Отклонить» → MainActivity с extra
 * `ntfry_call`, которая передаёт действие в JS (ChatWidget → voice_answer).
 * Шаблон из messenger/android/, копируется scripts/android-setup.cjs.
 */
class IncomingCallActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = this
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            @Suppress("DEPRECATION")
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
                    WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON or
                    WindowManager.LayoutParams.FLAG_DISMISS_KEYGUARD,
            )
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        val name = intent.getStringExtra(CallService.EXTRA_NAME) ?: "…"
        val room = intent.getStringExtra(CallService.EXTRA_ROOM) ?: ""
        val dp = resources.displayMetrics.density

        val rootView = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setBackgroundColor(Color.parseColor("#0B0F14"))
            setPadding((32 * dp).toInt(), 0, (32 * dp).toInt(), 0)
        }
        rootView.addView(TextView(this).apply {
            text = "NETFORY · ВХОДЯЩИЙ ЗВОНОК"
            setTextColor(Color.parseColor("#2DD4BF"))
            textSize = 13f
            letterSpacing = 0.16f
            typeface = Typeface.MONOSPACE
            gravity = Gravity.CENTER
        })
        rootView.addView(TextView(this).apply {
            text = name
            setTextColor(Color.WHITE)
            textSize = 30f
            typeface = Typeface.DEFAULT_BOLD
            gravity = Gravity.CENTER
            setPadding(0, (18 * dp).toInt(), 0, (64 * dp).toInt())
        })
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
        }
        row.addView(bigButton("ОТКЛОНИТЬ", "#EF4444", dp) { act(room, "decline") })
        row.addView(View(this), LinearLayout.LayoutParams((24 * dp).toInt(), 1))
        row.addView(bigButton("ОТВЕТИТЬ", "#22C55E", dp) { act(room, "answer") })
        rootView.addView(row)
        setContentView(rootView)
    }

    private fun bigButton(label: String, color: String, dp: Float, onClick: () -> Unit): Button =
        Button(this).apply {
            text = label
            setTextColor(Color.WHITE)
            setBackgroundColor(Color.parseColor(color))
            textSize = 15f
            typeface = Typeface.DEFAULT_BOLD
            minWidth = (140 * dp).toInt()
            minHeight = (64 * dp).toInt()
            setOnClickListener { onClick() }
        }

    private fun act(room: String, action: String) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            (getSystemService(KEYGUARD_SERVICE) as KeyguardManager).requestDismissKeyguard(this, null)
        }
        CallService.hideIncoming(this)
        startActivity(
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                .putExtra(CallService.EXTRA_ACTION, action)
                .putExtra(CallService.EXTRA_ROOM, room),
        )
        finish()
    }

    override fun onDestroy() {
        if (current === this) current = null
        super.onDestroy()
    }

    companion object {
        @Volatile
        var current: IncomingCallActivity? = null
    }
}
