package __PACKAGE__

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

/**
 * Foreground service NETFORY Messenger: держит процесс живым, когда приложение свёрнуто —
 * P2P-соединения (iroh) и активный голосовой звонок не обрываются системой.
 * Тип phoneCall (+ microphone, когда RECORD_AUDIO выдан — иначе Android 14 не даст микрофон в фоне).
 * Уведомление канала IMPORTANCE_MIN — без звука и без иконки в статус-баре.
 * Статические showIncoming/hideIncoming вызываются из Rust (JNI, chat/voice.rs → android.rs):
 * полноэкранное уведомление входящего звонка (fullScreenIntent → IncomingCallActivity поверх
 * блокировки, рингтон, кнопки «Ответить»/«Отклонить»).
 * Шаблон копируется в gen/android скриптом scripts/android-setup.cjs (пакет подставляется).
 */
class CallService : Service() {
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = NotificationChannel(CHANNEL, "NETFORY Messenger · P2P", NotificationManager.IMPORTANCE_MIN)
            ch.setShowBadge(false)
            ch.description = "Фоновая работа мессенджера: сообщения и звонки"
            (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(ch)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val n = buildNotification()
        val micGranted =
            checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                var type = ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL
                if (micGranted) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
                startForeground(ID, n, type)
            } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL)
            } else {
                startForeground(ID, n)
            }
        } catch (e: Exception) {
            // Нельзя стать foreground (ограничения ОС) — не держим «висящий» сервис: иначе ANR
            stopSelf()
            return START_NOT_STICKY
        }
        acquireWakeLock()
        return START_STICKY
    }

    override fun onDestroy() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) stopForeground(STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }

    private fun acquireWakeLock() {
        if (wakeLock?.isHeld == true) return
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ntfry:p2p").also { it.acquire() }
    }

    private fun buildNotification(): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
        return b
            .setContentTitle("NETFORY Messenger")
            .setContentText("P2P-соединение активно")
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentIntent(open)
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .setVisibility(Notification.VISIBILITY_SECRET)
            .build()
    }

    companion object {
        const val CHANNEL = "ntfry_p2p"
        const val ID = 7401
        const val CALL_CHANNEL = "ntfry_calls"
        const val CALL_ID = 7402
        const val EXTRA_NAME = "ntfry_name"
        const val EXTRA_ROOM = "ntfry_room"
        const val EXTRA_ACTION = "ntfry_call"

        private fun callChannel(ctx: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            if (nm.getNotificationChannel(CALL_CHANNEL) != null) return
            val ch = NotificationChannel(CALL_CHANNEL, "Входящие звонки", NotificationManager.IMPORTANCE_HIGH)
            ch.description = "Звонки NETFORY Messenger"
            ch.enableVibration(true)
            ch.vibrationPattern = longArrayOf(0, 600, 400, 600, 400, 600)
            ch.setSound(
                RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build(),
            )
            ch.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            nm.createNotificationChannel(ch)
        }

        private fun actionIntent(ctx: Context, action: String, room: String, code: Int): PendingIntent =
            PendingIntent.getActivity(
                ctx,
                code,
                Intent(ctx, MainActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                    .putExtra(EXTRA_ACTION, action)
                    .putExtra(EXTRA_ROOM, room),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )

        const val URI_SCHEME = "ntfry"

        /** Входящий звонок (из Rust): через Telecom (BT-гарнитура, маршрут звука; UI покажет
         *  onShowIncomingCallUi), при отказе Telecom — уведомление напрямую. */
        @JvmStatic
        fun showIncoming(ctx: Context, name: String, room: String) {
            val viaTelecom = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && NtfryConnectionService.incoming(ctx, name, room)
            if (!viaTelecom) showIncomingNotification(ctx, name, room)
        }

        /** Состояние звонка из Rust (voice.rs emit_state): calling | active | ended(+reason). */
        @JvmStatic
        fun callState(ctx: Context, room: String, state: String, reason: String, name: String) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
                if (state == "ended") hideIncoming(ctx)
                return
            }
            when (state) {
                "calling" -> NtfryConnectionService.outgoing(ctx, name, room)
                "active" -> {
                    hideIncoming(ctx)
                    NtfryConnectionService.active()
                }
                "ended" -> {
                    hideIncoming(ctx)
                    NtfryConnectionService.ended(reason)
                }
            }
        }

        /** Маршрут звука (из UI → Rust → сюда): speaker | earpiece | bluetooth | wired. */
        @JvmStatic
        fun audioRoute(ctx: Context, route: String) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) NtfryConnectionService.route(route)
            else try {
                val am = ctx.getSystemService(Context.AUDIO_SERVICE) as android.media.AudioManager
                am.isSpeakerphoneOn = route == "speaker"
            } catch (_: Exception) {
            }
        }

        /** Полноэкранное уведомление входящего с рингтоном и кнопками (без Telecom или из onShowIncomingCallUi). */
        @JvmStatic
        fun showIncomingNotification(ctx: Context, name: String, room: String) {
            try {
                callChannel(ctx)
                val full = PendingIntent.getActivity(
                    ctx,
                    3,
                    Intent(ctx, IncomingCallActivity::class.java)
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                        .putExtra(EXTRA_NAME, name)
                        .putExtra(EXTRA_ROOM, room),
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
                val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) Notification.Builder(ctx, CALL_CHANNEL) else Notification.Builder(ctx)
                b.setContentTitle("☎ Входящий звонок")
                    .setContentText(name)
                    .setSmallIcon(R.mipmap.ic_launcher)
                    .setCategory(Notification.CATEGORY_CALL)
                    .setPriority(Notification.PRIORITY_MAX)
                    .setOngoing(true)
                    .setAutoCancel(false)
                    .setVisibility(Notification.VISIBILITY_PUBLIC)
                    .setContentIntent(full)
                    .setFullScreenIntent(full, true)
                    .addAction(Notification.Action.Builder(null, "Отклонить", actionIntent(ctx, "decline", room, 1)).build())
                    .addAction(Notification.Action.Builder(null, "Ответить", actionIntent(ctx, "answer", room, 2)).build())
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) b.setTimeoutAfter(50_000)
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
                    b.setSound(RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE))
                    b.setVibrate(longArrayOf(0, 600, 400, 600, 400, 600))
                }
                (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(CALL_ID, b.build())
            } catch (_: Exception) {
            }
        }

        /** Звонок принят/отклонён/пропущен (из Rust): убрать уведомление и экран. */
        @JvmStatic
        fun hideIncoming(ctx: Context) {
            try {
                (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(CALL_ID)
            } catch (_: Exception) {
            }
            IncomingCallActivity.current?.let { a -> a.runOnUiThread { if (!a.isFinishing) a.finish() } }
        }

        fun start(ctx: Context) {
            val i = Intent(ctx, CallService::class.java)
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) ctx.startForegroundService(i) else ctx.startService(i)
            } catch (_: Exception) {
            }
        }

        fun stop(ctx: Context) {
            try {
                ctx.stopService(Intent(ctx, CallService::class.java))
            } catch (_: Exception) {
            }
        }
    }
}
