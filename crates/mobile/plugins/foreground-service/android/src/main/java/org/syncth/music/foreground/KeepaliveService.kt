package org.syncth.music.foreground

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log

/// Foreground-сервис, который не даёт Android заморозить приёмник.
///
/// Пока сервис жив, процесс получает foreground-важность, CPU удерживается
/// частичным wake lock, а Wi-Fi — high-perf lock. Уведомление показывает,
/// ждём ли мы ПК, и позволяет выключить сервис вручную.
class KeepaliveService : Service() {

    private val handler = Handler(Looper.getMainLooper())
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null

    private val tick = object : Runnable {
        override fun run() {
            if (
                activeSessions == 0 &&
                !appForeground &&
                SystemClock.elapsedRealtime() - lastActivity >= idleMinutes * 60_000L
            ) {
                Log.i(TAG, "нет подключений $idleMinutes мин — останавливаюсь")
                stopNow()
                return
            }
            handler.postDelayed(this, CHECK_INTERVAL_MS)
        }
    }

    override fun onCreate() {
        super.onCreate()
        instance = this
        createChannel()
        acquireLocks()
        publish()
        handler.postDelayed(tick, CHECK_INTERVAL_MS)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                // Ручное выключение: следующий запуск приложения снова включит
                // сервис, но до тех пор телефон не слушает порт.
                stopNow()
                return START_NOT_STICKY
            }
            ACTION_START -> {
                idleMinutes = clampIdle(intent.getIntExtra(EXTRA_IDLE, DEFAULT_IDLE_MINUTES))
                lastActivity = SystemClock.elapsedRealtime()
                publish()
            }
            else -> publish()
        }
        refreshNotification()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacks(tick)
        releaseLocks()
        instance = null
        try {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } catch (error: RuntimeException) {
            Log.w(TAG, "stopForeground: ${error.message}")
        }
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun publish() {
        val notification = buildNotification()
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
                )
            } else {
                startForeground(NOTIFICATION_ID, notification)
            }
        } catch (error: RuntimeException) {
            // Android 12+ может запретить старт из фона, если разрешения или
            // квоты не хватает. Лучше тихо остановиться, чем упасть.
            Log.w(TAG, "не удалось стать foreground-сервисом: ${error.message}")
            stopNow()
        }
    }

    private fun refreshNotification() {
        if (instance !== this) return
        try {
            val manager = getSystemService(NotificationManager::class.java)
            manager.notify(NOTIFICATION_ID, buildNotification())
        } catch (error: RuntimeException) {
            Log.w(TAG, "не удалось обновить уведомление: ${error.message}")
        }
    }

    private fun buildNotification(): Notification {
        val flags = PendingIntent.FLAG_UPDATE_CURRENT or
            if (Build.VERSION.SDK_INT >= 23) PendingIntent.FLAG_IMMUTABLE else 0

        val launch = packageManager.getLaunchIntentForPackage(packageName)
        val contentIntent = launch?.let { PendingIntent.getActivity(this, 0, it, flags) }
        val stopIntent = Intent(this, KeepaliveService::class.java).setAction(ACTION_STOP)
        val stopAction = PendingIntent.getService(this, 1, stopIntent, flags)

        val text = when {
            activeSessions > 0 -> "Компьютер подключён"
            appForeground -> "Приложение открыто"
            else -> "Ожидание компьютера"
        }

        val builder = if (Build.VERSION.SDK_INT >= 26) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this).setPriority(Notification.PRIORITY_LOW)
        }
        builder
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("SynctMusic · приём включён")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setCategory(Notification.CATEGORY_SERVICE)
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                "Отключить",
                stopAction,
            )
        if (contentIntent != null) builder.setContentIntent(contentIntent)

        return builder.build()
    }

    private fun createChannel() {
        if (Build.VERSION.SDK_INT < 26) return
        val manager = getSystemService(NotificationManager::class.java)
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Синхронизация",
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Приём файлов с компьютера, пока приложение в фоне"
            setShowBadge(false)
            lockscreenVisibility = Notification.VISIBILITY_PRIVATE
        }
        manager.createNotificationChannel(channel)
    }

    private fun acquireLocks() {
        try {
            if (wakeLock == null) {
                val power = getSystemService(Context.POWER_SERVICE) as PowerManager
                wakeLock = power
                    .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "$TAG:sync")
                    .apply {
                        setReferenceCounted(false)
                        acquire()
                    }
            }
            if (wifiLock == null) {
                val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
                @Suppress("DEPRECATION")
                wifiLock = wifi
                    .createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "$TAG:sync")
                    .apply {
                        setReferenceCounted(false)
                        acquire()
                    }
            }
        } catch (error: RuntimeException) {
            // Блокировки — оптимизация. Без них сервис всё равно полезен.
            Log.w(TAG, "не удалось взять блокировки: ${error.message}")
        }
    }

    private fun releaseLocks() {
        try {
            wakeLock?.takeIf { it.isHeld }?.release()
        } catch (error: RuntimeException) {
            Log.w(TAG, "wake lock: ${error.message}")
        }
        wakeLock = null
        try {
            wifiLock?.takeIf { it.isHeld }?.release()
        } catch (error: RuntimeException) {
            Log.w(TAG, "wifi lock: ${error.message}")
        }
        wifiLock = null
    }

    private fun stopNow() {
        try {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } catch (error: RuntimeException) {
            Log.w(TAG, "stopForeground: ${error.message}")
        }
        stopSelf()
    }

    companion object {
        const val DEFAULT_IDLE_MINUTES = 25

        private const val TAG = "SynctMusicFgs"
        private const val CHANNEL_ID = "syncth-sync"
        private const val NOTIFICATION_ID = 0x5343
        private const val CHECK_INTERVAL_MS = 60_000L
        private const val ACTION_START = "org.syncth.music.foreground.START"
        private const val ACTION_STOP = "org.syncth.music.foreground.STOP"
        private const val EXTRA_IDLE = "idle_minutes"

        /// Сколько сеансов с ПК идёт прямо сейчас. Пока счётчик больше нуля,
        /// таймер простоя не тикает.
        @Volatile
        private var activeSessions: Int = 0

        @Volatile
        private var appForeground: Boolean = true

        @Volatile
        private var lastActivity: Long = SystemClock.elapsedRealtime()

        @Volatile
        private var idleMinutes: Int = DEFAULT_IDLE_MINUTES

        @Volatile
        private var instance: KeepaliveService? = null

        fun start(context: Context, minutes: Int) {
            idleMinutes = clampIdle(minutes)
            if (instance == null) activeSessions = 0
            appForeground = true
            lastActivity = SystemClock.elapsedRealtime()
            val intent = Intent(context, KeepaliveService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_IDLE, idleMinutes)
            try {
                if (Build.VERSION.SDK_INT >= 26) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            } catch (error: RuntimeException) {
                Log.w(TAG, "не удалось запустить сервис: ${error.message}")
            }
        }

        fun stop(context: Context) {
            val current = instance
            if (current != null) {
                current.stopNow()
            } else {
                context.stopService(Intent(context, KeepaliveService::class.java))
            }
        }

        fun touch(active: Boolean) {
            activeSessions = (activeSessions + if (active) 1 else -1).coerceAtLeast(0)
            lastActivity = SystemClock.elapsedRealtime()
            instance?.refreshNotification()
        }

        fun setForeground(foreground: Boolean) {
            appForeground = foreground
            lastActivity = SystemClock.elapsedRealtime()
            instance?.refreshNotification()
        }

        fun status(): Snapshot = Snapshot(
            running = instance != null,
            foreground = appForeground,
            activeSessions = activeSessions,
            idleMinutes = idleMinutes,
        )

        private fun clampIdle(minutes: Int): Int = minutes.coerceIn(5, 120)
    }

    data class Snapshot(
        val running: Boolean,
        val foreground: Boolean,
        val activeSessions: Int,
        val idleMinutes: Int,
    )
}
