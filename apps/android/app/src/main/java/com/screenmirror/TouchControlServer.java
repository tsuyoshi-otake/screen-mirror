package com.screenmirror;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetSocketAddress;
import java.net.SocketException;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;

final class TouchControlServer {
    private static final int CONTROL_PORT = 47778;
    private static final String PROTOCOL = "screen-mirror.control";
    private static final int VERSION = 1;
    private static final int SOCKET_BUFFER_SIZE = 512 * 1024;

    private final AtomicBoolean running = new AtomicBoolean(false);
    private final byte[] buffer = new byte[2048];
    private final DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
    private volatile DatagramSocket socket;
    private Thread thread;
    private String expectedPinHash;

    synchronized void start(String pin) throws Exception {
        stop();
        expectedPinHash = Pin.hash(pin);
        DatagramSocket localSocket = new DatagramSocket(null);
        try {
            localSocket.setReuseAddress(true);
            localSocket.setReceiveBufferSize(SOCKET_BUFFER_SIZE);
            localSocket.setSoTimeout(250);
            localSocket.bind(new InetSocketAddress(CONTROL_PORT));
        } catch (Exception error) {
            localSocket.close();
            throw error;
        }
        socket = localSocket;
        running.set(true);
        thread = new Thread(() -> receiveLoop(localSocket), "touch-control-server");
        thread.start();
    }

    synchronized void stop() {
        running.set(false);
        DatagramSocket activeSocket = socket;
        if (activeSocket != null) {
            activeSocket.close();
        }
        if (thread != null) {
            thread.interrupt();
            thread = null;
        }
    }

    boolean isInjectingEnabled() {
        return ScreenMirrorAccessibilityService.isConnected();
    }

    private void receiveLoop(DatagramSocket localSocket) {
        try (DatagramSocket activeSocket = localSocket) {
            while (running.get()) {
                try {
                    packet.setData(buffer, 0, buffer.length);
                    activeSocket.receive(packet);
                    TouchEvent event = decode(packet.getData(), packet.getOffset(), packet.getLength());
                    if (event != null) {
                        ScreenMirrorAccessibilityService.dispatchTouch(event);
                    }
                } catch (SocketTimeoutException ignored) {
                } catch (SocketException error) {
                    if (running.get()) {
                        throw error;
                    }
                }
            }
        } catch (Exception ignored) {
        } finally {
            if (socket == localSocket) {
                socket = null;
            }
        }
    }

    private TouchEvent decode(byte[] data, int offset, int length) {
        try {
            JSONObject json = new JSONObject(new String(data, offset, length, StandardCharsets.UTF_8));
            if (!PROTOCOL.equals(json.optString("protocol")) || json.optInt("version") != VERSION) {
                return null;
            }
            if (!expectedPinHash.equals(json.optString("pin_hash", ""))) {
                return null;
            }
            return new TouchEvent(
                    json.getString("action"),
                    clamp((float) json.getDouble("x")),
                    clamp((float) json.getDouble("y")),
                    json.optInt("pointer_id", 0)
            );
        } catch (Exception ignored) {
            return null;
        }
    }

    private static float clamp(float value) {
        return Math.max(0.0f, Math.min(1.0f, value));
    }

    static final class TouchEvent {
        final String action;
        final float x;
        final float y;
        final int pointerId;

        TouchEvent(String action, float x, float y, int pointerId) {
            this.action = action;
            this.x = x;
            this.y = y;
            this.pointerId = pointerId;
        }
    }
}
