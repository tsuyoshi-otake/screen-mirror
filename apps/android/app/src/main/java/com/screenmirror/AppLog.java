package com.screenmirror;

import android.content.Context;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.os.Build;
import android.util.Log;

import java.util.ArrayDeque;

final class AppLog {
    private static final String TAG = "ScreenMirror";
    private static final int MAX_ENTRIES = 200;
    private static final ArrayDeque<String> ENTRIES = new ArrayDeque<>(MAX_ENTRIES);

    private AppLog() {
    }

    static void info(String message) {
        Log.i(TAG, message);
        append("INFO", message, null);
    }

    static void warn(String message, Throwable error) {
        Log.w(TAG, message, error);
        append("WARN", message, error);
    }

    static void error(String message, Throwable error) {
        Log.e(TAG, message, error);
        append("ERROR", message, error);
    }

    static synchronized String diagnostics(Context context, String sessionState) {
        Runtime runtime = Runtime.getRuntime();
        long usedBytes = runtime.totalMemory() - runtime.freeMemory();
        StringBuilder report = new StringBuilder();
        report.append("Screen Mirror Android diagnostics\n")
                .append("app_version=").append(appVersion(context)).append('\n')
                .append("device=").append(Build.MANUFACTURER).append(' ').append(Build.MODEL).append('\n')
                .append("android_sdk=").append(Build.VERSION.SDK_INT).append('\n')
                .append("session=").append(sessionState).append('\n')
                .append("java_heap_used_mib=").append(toMiB(usedBytes)).append('\n')
                .append("java_heap_committed_mib=").append(toMiB(runtime.totalMemory())).append('\n')
                .append("java_heap_max_mib=").append(toMiB(runtime.maxMemory())).append('\n')
                .append("\nRecent events:\n");
        for (String entry : ENTRIES) {
            report.append(entry).append('\n');
        }
        return report.toString();
    }

    private static synchronized void append(String level, String message, Throwable error) {
        StringBuilder entry = new StringBuilder()
                .append(System.currentTimeMillis())
                .append(' ')
                .append(level)
                .append(' ')
                .append(message);
        if (error != null) {
            entry.append(": ").append(error.getClass().getSimpleName());
            if (error.getMessage() != null) {
                entry.append(": ").append(error.getMessage());
            }
            if (error.getStackTrace().length > 0) {
                entry.append(" @ ").append(error.getStackTrace()[0]);
            }
        }
        while (ENTRIES.size() >= MAX_ENTRIES) {
            ENTRIES.removeFirst();
        }
        ENTRIES.addLast(entry.toString());
    }

    private static long toMiB(long bytes) {
        return bytes / (1024L * 1024L);
    }

    @SuppressWarnings("deprecation")
    private static String appVersion(Context context) {
        try {
            PackageInfo info;
            if (Build.VERSION.SDK_INT >= 33) {
                info = context.getPackageManager().getPackageInfo(
                        context.getPackageName(),
                        PackageManager.PackageInfoFlags.of(0)
                );
            } else {
                info = context.getPackageManager().getPackageInfo(context.getPackageName(), 0);
            }
            return info.versionName == null ? "unknown" : info.versionName;
        } catch (PackageManager.NameNotFoundException error) {
            return "unknown";
        }
    }
}
