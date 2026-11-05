package org.syncth.music.foreground

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class StartArgs {
    var idleMinutes: Int = KeepaliveService.DEFAULT_IDLE_MINUTES
}

@InvokeArg
class TouchArgs {
    var active: Boolean = false
}

@InvokeArg
class ForegroundArgs {
    var foreground: Boolean = true
}

@TauriPlugin
class ForegroundServicePlugin(private val activity: Activity) : Plugin(activity) {

    @Command
    fun start(invoke: Invoke) {
        val args = invoke.parseArgs(StartArgs::class.java)
        requestNotifications()
        KeepaliveService.start(activity.applicationContext, args.idleMinutes)
        invoke.resolve(status())
    }

    @Command
    fun stop(invoke: Invoke) {
        KeepaliveService.stop(activity.applicationContext)
        invoke.resolve(status())
    }

    @Command
    fun touch(invoke: Invoke) {
        val args = invoke.parseArgs(TouchArgs::class.java)
        KeepaliveService.touch(args.active)
        invoke.resolve(status())
    }

    @Command
    fun setForeground(invoke: Invoke) {
        val args = invoke.parseArgs(ForegroundArgs::class.java)
        KeepaliveService.setForeground(args.foreground)
        invoke.resolve(status())
    }

    @Command
    fun status(invoke: Invoke) {
        invoke.resolve(status())
    }

    /// Android 13+ скрывает уведомление сервиса без явного согласия. Без него
    /// кнопка «Отключить» недоступна, поэтому спрашиваем один раз при старте.
    private fun requestNotifications() {
        if (Build.VERSION.SDK_INT < 33) return
        if (activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0x5343)
    }

    private fun status(): JSObject {
        val snapshot = KeepaliveService.status()
        return JSObject().apply {
            put("running", snapshot.running)
            put("foreground", snapshot.foreground)
            put("activeSessions", snapshot.activeSessions)
            put("idleMinutes", snapshot.idleMinutes)
        }
    }
}
