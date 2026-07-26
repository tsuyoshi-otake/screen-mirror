package com.screenmirror;

import android.os.Build;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;

final class DiscoveryAgent {
    static final int DISCOVERY_PORT = 47777;
    private static final String PROTOCOL = "screen-mirror.discovery";
    private static final int VERSION = 1;

    static final class Peer {
        final String instanceId;
        final String deviceName;
        final String role;
        final String host;
        final int streamPort;
        final int audioPort;
        final int displayWidth;
        final int displayHeight;
        final int refreshHz;

        Peer(String instanceId, String deviceName, String role, String host, int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz) {
            this.instanceId = instanceId;
            this.deviceName = deviceName;
            this.role = role;
            this.host = host;
            this.streamPort = streamPort;
            this.audioPort = audioPort;
            this.displayWidth = displayWidth;
            this.displayHeight = displayHeight;
            this.refreshHz = refreshHz;
        }

        @Override
        public String toString() {
            return deviceName + " " + host + ":" + streamPort + " (" + role + ")";
        }
    }

    private final String instanceId = Build.MODEL.replace(' ', '-') + "-" + UUID.randomUUID();
    private final AtomicBoolean running = new AtomicBoolean(false);
    private Thread thread;

    void startReceiverBeacon(int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz, String pin) {
        stop();
        running.set(true);
        String normalizedPin = Pin.normalize(pin);
        thread = new Thread(() -> runBeacon("receiver", streamPort, audioPort, displayWidth, displayHeight, refreshHz, normalizedPin), "discovery-beacon");
        thread.start();
    }

    void stop() {
        running.set(false);
        if (thread != null) {
            thread.interrupt();
            thread = null;
        }
    }

    List<Peer> discoverReceivers(long timeoutMs, String pin) throws Exception {
        ArrayList<Peer> peers = new ArrayList<>();
        long deadline = System.currentTimeMillis() + timeoutMs;
        byte[] buffer = new byte[2048];
        String expectedPinHash = Pin.hash(pin);

        try (DatagramSocket socket = new DatagramSocket(null)) {
            socket.setReuseAddress(true);
            socket.bind(new InetSocketAddress(DISCOVERY_PORT));
            socket.setSoTimeout(250);

            while (System.currentTimeMillis() < deadline) {
                try {
                    DatagramPacket packet = new DatagramPacket(buffer, buffer.length);
                    socket.receive(packet);
                    Peer peer = decodePeer(packet, expectedPinHash);
                    if (!"receiver".equals(peer.role)) {
                        continue;
                    }
                    boolean exists = false;
                    for (Peer existing : peers) {
                        if (existing.instanceId.equals(peer.instanceId)) {
                            exists = true;
                            break;
                        }
                    }
                    if (!exists) {
                        peers.add(peer);
                    }
                } catch (SocketTimeoutException ignored) {
                }
            }
        }

        return peers;
    }

    private void runBeacon(String role, int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz, String pin) {
        try (DatagramSocket socket = new DatagramSocket()) {
            socket.setBroadcast(true);
            while (running.get()) {
                byte[] payload = announcement(role, streamPort, audioPort, displayWidth, displayHeight, refreshHz, pin);
                DatagramPacket packet = new DatagramPacket(
                        payload,
                        payload.length,
                        InetAddress.getByName("255.255.255.255"),
                        DISCOVERY_PORT
                );
                socket.send(packet);
                Thread.sleep(1000);
            }
        } catch (Exception ignored) {
        }
    }

    private byte[] announcement(String role, int streamPort, int audioPort, int displayWidth, int displayHeight, int refreshHz, String pin) throws Exception {
        JSONObject json = new JSONObject();
        json.put("protocol", PROTOCOL);
        json.put("version", VERSION);
        json.put("instance_id", instanceId);
        json.put("device_name", Build.MODEL);
        json.put("role", role);
        json.put("stream_port", streamPort);
        json.put("audio_port", audioPort);
        json.put("pin_hash", Pin.hash(pin));
        JSONObject display = new JSONObject();
        display.put("width", displayWidth);
        display.put("height", displayHeight);
        if (refreshHz > 0) {
            display.put("refresh_hz", refreshHz);
        }
        json.put("display", display);
        json.put("timestamp_ms", System.currentTimeMillis());
        return json.toString().getBytes(StandardCharsets.UTF_8);
    }

    private static Peer decodePeer(DatagramPacket packet, String expectedPinHash) throws Exception {
        String text = new String(packet.getData(), packet.getOffset(), packet.getLength(), StandardCharsets.UTF_8);
        JSONObject json = new JSONObject(text);
        if (!PROTOCOL.equals(json.optString("protocol")) || json.optInt("version") != VERSION) {
            throw new IllegalArgumentException("unsupported discovery packet");
        }
        if (!expectedPinHash.equals(json.optString("pin_hash", ""))) {
            throw new IllegalArgumentException("PIN mismatch");
        }
        InetAddress address = packet.getAddress();
        if (!(address instanceof Inet4Address)) {
            throw new IllegalArgumentException("only IPv4 discovery is supported");
        }
        JSONObject display = json.optJSONObject("display");
        int displayWidth = display == null ? 0 : display.optInt("width", 0);
        int displayHeight = display == null ? 0 : display.optInt("height", 0);
        int refreshHz = display == null ? 0 : display.optInt("refresh_hz", 0);
        int streamPort = json.getInt("stream_port");

        return new Peer(
                json.getString("instance_id"),
                json.optString("device_name", "Android"),
                json.getString("role"),
                address.getHostAddress(),
                streamPort,
                json.optInt("audio_port", streamPort + 1),
                displayWidth,
                displayHeight,
                refreshHz
        );
    }
}
