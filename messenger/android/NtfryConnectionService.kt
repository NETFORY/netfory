package __PACKAGE__

import android.content.ComponentName
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.telecom.CallAudioState
import android.telecom.Connection
import android.telecom.ConnectionRequest
import android.telecom.ConnectionService
import android.telecom.DisconnectCause
import android.telecom.PhoneAccount
import android.telecom.PhoneAccountHandle
import android.telecom.TelecomManager
import androidx.annotation.RequiresApi

/**
 * Self-managed ConnectionService (API 26+): регистрирует звонки NETFORY в системном Telecom.
 * Что даёт: маршрутизация звука (Bluetooth SCO / динамик / разговорный динамик) средствами ОС,
 * кнопки гарнитуры «ответить/завершить» (onAnswer/onDisconnect), координация с сотовыми
 * звонками (наш звонок ставится на удержание/прерывается корректно), системный индикатор звонка.
 * UI входящего остаётся нашим (self-managed): onShowIncomingCallUi → полноэкранное уведомление.
 * Состояния приходят из Rust (voice.rs emit_state → android.rs → CallService.callState).
 * Шаблон из messenger/android/, копируется scripts/android-setup.cjs.
 */
@RequiresApi(Build.VERSION_CODES.O)
class NtfryConnectionService : ConnectionService() {
    override fun onCreateIncomingConnection(account: PhoneAccountHandle?, request: ConnectionRequest?): Connection {
        val ex = request?.extras ?: Bundle()
        val inner = ex.getBundle(TelecomManager.EXTRA_INCOMING_CALL_EXTRAS)
        val room = ex.getString(CallService.EXTRA_ROOM) ?: inner?.getString(CallService.EXTRA_ROOM) ?: ""
        val name = ex.getString(CallService.EXTRA_NAME) ?: inner?.getString(CallService.EXTRA_NAME) ?: "…"
        val c = NtfryConnection(applicationContext, room, name, incoming = true)
        c.setRinging()
        current = c
        return c
    }

    override fun onCreateOutgoingConnection(account: PhoneAccountHandle?, request: ConnectionRequest?): Connection {
        val ex = request?.extras ?: Bundle()
        val inner = ex.getBundle(TelecomManager.EXTRA_OUTGOING_CALL_EXTRAS)
        val room = ex.getString(CallService.EXTRA_ROOM) ?: inner?.getString(CallService.EXTRA_ROOM) ?: ""
        val name = ex.getString(CallService.EXTRA_NAME) ?: inner?.getString(CallService.EXTRA_NAME) ?: "…"
        val c = NtfryConnection(applicationContext, room, name, incoming = false)
        c.setDialing()
        current = c
        return c
    }

    override fun onCreateIncomingConnectionFailed(account: PhoneAccountHandle?, request: ConnectionRequest?) {
        // Telecom не пустил (например, идёт сотовый звонок) — показываем своё уведомление напрямую
        val ex = request?.extras ?: Bundle()
        CallService.showIncomingNotification(
            applicationContext,
            ex.getString(CallService.EXTRA_NAME) ?: "…",
            ex.getString(CallService.EXTRA_ROOM) ?: "",
        )
    }

    override fun onCreateOutgoingConnectionFailed(account: PhoneAccountHandle?, request: ConnectionRequest?) {
        // звонок продолжится без Telecom (без BT-маршрутизации) — ничего не делаем
    }

    class NtfryConnection(private val ctx: Context, val room: String, val name: String, val incoming: Boolean) : Connection() {
        init {
            connectionProperties = PROPERTY_SELF_MANAGED
            connectionCapabilities = CAPABILITY_MUTE or CAPABILITY_SUPPORT_HOLD or CAPABILITY_HOLD
            audioModeIsVoip = true
            setCallerDisplayName(name, TelecomManager.PRESENTATION_ALLOWED)
            setAddress(Uri.fromParts(CallService.URI_SCHEME, room, null), TelecomManager.PRESENTATION_ALLOWED)
        }

        override fun onShowIncomingCallUi() {
            CallService.showIncomingNotification(ctx, name, room)
        }

        /** Кнопка гарнитуры / системный UI: ответить → JS voice_answer(true). */
        override fun onAnswer() {
            MainActivity.deliverCallAction(ctx, "answer", room)
        }

        override fun onAnswer(videoState: Int) = onAnswer()

        override fun onReject() {
            MainActivity.deliverCallAction(ctx, "decline", room)
        }

        /** Завершить (гарнитура / сотовый звонок прервал) → JS voice_hangup. */
        override fun onDisconnect() {
            MainActivity.deliverCallAction(ctx, "hangup", room)
            setDisconnected(DisconnectCause(DisconnectCause.LOCAL))
            destroy()
            if (current === this) current = null
        }

        override fun onAbort() = onDisconnect()

        override fun onHold() {
            setOnHold()
            MainActivity.deliverCallAction(ctx, "mute", room)
        }

        override fun onUnhold() {
            setActive()
            MainActivity.deliverCallAction(ctx, "unmute", room)
        }

        override fun onCallAudioStateChanged(state: CallAudioState?) {
            lastRoute = state?.route ?: lastRoute
        }

        companion object {
            @Volatile
            var lastRoute: Int = CallAudioState.ROUTE_EARPIECE
        }
    }

    companion object {
        @Volatile
        var current: NtfryConnection? = null

        fun handle(ctx: Context) = PhoneAccountHandle(ComponentName(ctx, NtfryConnectionService::class.java), "ntfry")

        /** Регистрация self-managed аккаунта (идемпотентно). false — Telecom недоступен. */
        fun register(ctx: Context): Boolean {
            return try {
                val tm = ctx.getSystemService(Context.TELECOM_SERVICE) as TelecomManager
                val h = handle(ctx)
                if (tm.getPhoneAccount(h) == null) {
                    val acc = PhoneAccount.builder(h, "NETFORY Messenger")
                        .setCapabilities(PhoneAccount.CAPABILITY_SELF_MANAGED)
                        .setSupportedUriSchemes(listOf(CallService.URI_SCHEME, PhoneAccount.SCHEME_SIP))
                        .build()
                    tm.registerPhoneAccount(acc)
                }
                true
            } catch (_: Exception) {
                false
            }
        }

        /** Входящий через Telecom. false — не удалось (вызывающий покажет уведомление сам). */
        fun incoming(ctx: Context, name: String, room: String): Boolean {
            if (!register(ctx)) return false
            return try {
                val tm = ctx.getSystemService(Context.TELECOM_SERVICE) as TelecomManager
                val h = handle(ctx)
                if (!tm.isIncomingCallPermitted(h)) return false
                val ex = Bundle()
                ex.putParcelable(TelecomManager.EXTRA_INCOMING_CALL_ADDRESS, Uri.fromParts(CallService.URI_SCHEME, room, null))
                ex.putString(CallService.EXTRA_NAME, name)
                ex.putString(CallService.EXTRA_ROOM, room)
                tm.addNewIncomingCall(h, ex)
                true
            } catch (_: Exception) {
                false
            }
        }

        /** Исходящий через Telecom (мы уже звоним по P2P; Telecom — для аудио-маршрута и гарнитуры). */
        fun outgoing(ctx: Context, name: String, room: String) {
            if (!register(ctx)) return
            try {
                val tm = ctx.getSystemService(Context.TELECOM_SERVICE) as TelecomManager
                val h = handle(ctx)
                if (!tm.isOutgoingCallPermitted(h)) return
                val ex = Bundle()
                ex.putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, h)
                val inner = Bundle()
                inner.putString(CallService.EXTRA_NAME, name)
                inner.putString(CallService.EXTRA_ROOM, room)
                ex.putBundle(TelecomManager.EXTRA_OUTGOING_CALL_EXTRAS, inner)
                tm.placeCall(Uri.fromParts(CallService.URI_SCHEME, room, null), ex)
            } catch (_: Exception) {
            }
        }

        fun active() {
            current?.let { if (it.state != Connection.STATE_ACTIVE) it.setActive() }
        }

        fun ended(reason: String) {
            val c = current ?: return
            current = null
            val cause = when (reason) {
                "missed" -> DisconnectCause.MISSED
                "declined-local", "declined" -> DisconnectCause.REJECTED
                "busy" -> DisconnectCause.BUSY
                "remote-hangup" -> DisconnectCause.REMOTE
                else -> DisconnectCause.LOCAL
            }
            try {
                c.setDisconnected(DisconnectCause(cause))
                c.destroy()
            } catch (_: Exception) {
            }
        }

        /** Маршрут звука: speaker | earpiece | bluetooth | wired. */
        fun route(name: String) {
            val c = current ?: return
            val r = when (name) {
                "speaker" -> CallAudioState.ROUTE_SPEAKER
                "bluetooth" -> CallAudioState.ROUTE_BLUETOOTH
                "wired" -> CallAudioState.ROUTE_WIRED_HEADSET
                else -> CallAudioState.ROUTE_EARPIECE
            }
            try {
                c.setAudioRoute(r)
            } catch (_: Exception) {
            }
        }
    }
}
