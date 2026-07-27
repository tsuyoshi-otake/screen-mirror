package com.screenmirror;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.Iterator;
import java.util.concurrent.atomic.AtomicBoolean;

final class ControlClient implements AutoCloseable {
    private static final int CONTROL_PORT = 47778;
    private static final String PROTOCOL = "screen-mirror.control";
    private static final int VERSION = 1;
    private static final int MAX_PENDING_EVENTS = 64;
    private static final long ERROR_LOG_INTERVAL_MS = 5000L;

    private final Object queueLock = new Object();
    private final ArrayDeque<OutboundEvent> queue = new ArrayDeque<>(MAX_PENDING_EVENTS);
    private final AtomicBoolean running = new AtomicBoolean(true);
    private final Thread worker;

    private volatile DatagramSocket socket;
    private String cachedHost;
    private InetAddress cachedAddress;
    private long lastErrorLogMs;

    ControlClient() {
        worker = new Thread(this::sendLoop, "touch-control-send");
        worker.start();
    }

    void send(String host, String action, float x, float y, int pointerId, String pin) {
        if (!running.get() || host == null || host.isEmpty()) {
            return;
        }
        OutboundEvent event = new OutboundEvent(host, action, clamp(x), clamp(y), pointerId, pin);
        synchronized (queueLock) {
            if ("move".equals(action) && !queue.isEmpty() && "move".equals(queue.peekLast().action)) {
                queue.removeLast();
            }
            if (queue.size() >= MAX_PENDING_EVENTS) {
                removeOldestMoveOrEvent();
            }
            queue.addLast(event);
            queueLock.notifyAll();
        }
    }

    @Override
    public void close() {
        if (!running.getAndSet(false)) {
            return;
        }
        synchronized (queueLock) {
            queue.clear();
            queueLock.notifyAll();
        }
        DatagramSocket activeSocket = socket;
        if (activeSocket != null) {
            activeSocket.close();
        }
        worker.interrupt();
        try {
            worker.join(1000L);
            if (worker.isAlive()) {
                AppLog.warn("touch sender thread did not stop within one second", null);
            }
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            AppLog.warn("interrupted while stopping touch sender", error);
        }
    }

    private void sendLoop() {
        try {
            while (running.get()) {
                OutboundEvent event = takeNext();
                if (event == null) {
                    continue;
                }
                try {
                    sendNow(event);
                } catch (Exception error) {
                    long now = System.currentTimeMillis();
                    if (now - lastErrorLogMs >= ERROR_LOG_INTERVAL_MS) {
                        lastErrorLogMs = now;
                        AppLog.warn("touch control send failed", error);
                    }
                }
            }
        } finally {
            DatagramSocket activeSocket = socket;
            socket = null;
            if (activeSocket != null) {
                activeSocket.close();
            }
        }
    }

    private OutboundEvent takeNext() {
        synchronized (queueLock) {
            while (running.get() && queue.isEmpty()) {
                try {
                    queueLock.wait();
                } catch (InterruptedException error) {
                    if (!running.get()) {
                        return null;
                    }
                }
            }
            return queue.pollFirst();
        }
    }

    private void sendNow(OutboundEvent event) throws Exception {
        JSONObject json = new JSONObject();
        json.put("protocol", PROTOCOL);
        json.put("version", VERSION);
        json.put("pin_hash", Pin.hash(event.pin));
        json.put("action", event.action);
        json.put("x", event.x);
        json.put("y", event.y);
        json.put("pointer_id", event.pointerId);
        json.put("timestamp_ms", System.currentTimeMillis());
        byte[] payload = json.toString().getBytes(StandardCharsets.UTF_8);
        DatagramSocket activeSocket = socket();
        activeSocket.send(new DatagramPacket(
                payload,
                payload.length,
                address(event.host),
                CONTROL_PORT
        ));
    }

    private DatagramSocket socket() throws Exception {
        DatagramSocket activeSocket = socket;
        if (activeSocket == null || activeSocket.isClosed()) {
            activeSocket = new DatagramSocket();
            socket = activeSocket;
        }
        return activeSocket;
    }

    private InetAddress address(String host) throws Exception {
        if (!host.equals(cachedHost) || cachedAddress == null) {
            cachedAddress = InetAddress.getByName(host);
            cachedHost = host;
        }
        return cachedAddress;
    }

    private void removeOldestMoveOrEvent() {
        Iterator<OutboundEvent> iterator = queue.iterator();
        while (iterator.hasNext()) {
            if ("move".equals(iterator.next().action)) {
                iterator.remove();
                return;
            }
        }
        queue.pollFirst();
    }

    private static float clamp(float value) {
        return Math.max(0.0f, Math.min(1.0f, value));
    }

    private static final class OutboundEvent {
        final String host;
        final String action;
        final float x;
        final float y;
        final int pointerId;
        final String pin;

        OutboundEvent(String host, String action, float x, float y, int pointerId, String pin) {
            this.host = host;
            this.action = action;
            this.x = x;
            this.y = y;
            this.pointerId = pointerId;
            this.pin = pin;
        }
    }
}
