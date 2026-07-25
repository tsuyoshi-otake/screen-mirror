package com.screenmirror;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;

final class ControlClient {
    private static final int CONTROL_PORT = 47778;
    private static final String PROTOCOL = "screen-mirror.control";
    private static final int VERSION = 1;

    void send(String host, String action, float x, float y, int pointerId) {
        new Thread(() -> {
            try (DatagramSocket socket = new DatagramSocket()) {
                JSONObject json = new JSONObject();
                json.put("protocol", PROTOCOL);
                json.put("version", VERSION);
                json.put("action", action);
                json.put("x", clamp(x));
                json.put("y", clamp(y));
                json.put("pointer_id", pointerId);
                json.put("timestamp_ms", System.currentTimeMillis());
                byte[] payload = json.toString().getBytes(StandardCharsets.UTF_8);
                socket.send(new DatagramPacket(
                        payload,
                        payload.length,
                        InetAddress.getByName(host),
                        CONTROL_PORT
                ));
            } catch (Exception ignored) {
            }
        }, "touch-control-send").start();
    }

    private static float clamp(float value) {
        return Math.max(0.0f, Math.min(1.0f, value));
    }
}
