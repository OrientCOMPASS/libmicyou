// libmicyou — Flutter (Material 3) reference frontend.
// Connects to micyou-daemon over WebSocket JSON-RPC (ws://host/rpc).
// Material 3 design language: NavigationRail, seed-generated color scheme,
// Material cards/switches/sliders, CustomPaint spectrum — the Flutter
// identity of the frontend family (not a clone of the other shells).

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart' show debugPrint;
import 'package:flutter/material.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

void main() {
  runApp(const MicYouApp());
}

/* ══════════════════════ JSON-RPC client ══════════════════════ */

class RpcError implements Exception {
  RpcError(this.code, this.message);
  final int code;
  final String message;
  @override
  String toString() => message;
}

/// Line-oriented JSON transport. Desktop builds talk to a spawned
/// micyou-daemon over stdio (the real-desktop-app pattern, same as the
/// Tauri/Qt frontends); a WebSocket transport remains for remote backends.
abstract class JsonTransport {
  void send(String line);
  Stream<String> get lines;
  Future<void> close();
}

class WsTransport implements JsonTransport {
  WsTransport(this._channel);
  final WebSocketChannel _channel;

  @override
  void send(String line) => _channel.sink.add(line);

  @override
  Stream<String> get lines => _channel.stream.cast<String>();

  @override
  Future<void> close() => _channel.sink.close();
}

class StdioTransport implements JsonTransport {
  StdioTransport(this._process) {
    _lineStream =
        _process.stdout.transform(utf8.decoder).transform(const LineSplitter());
    _process.stderr
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen((l) {
      // Daemon logs normally go to its file; surface stray stderr lines.
      debugPrint('daemon: $l');
    });
  }

  final Process _process;
  late final Stream<String> _lineStream;

  @override
  void send(String line) {
    _process.stdin.write('$line\n');
    unawaited(_process.stdin.flush());
  }

  @override
  Stream<String> get lines => _lineStream;

  @override
  Future<void> close() async {
    // EOF on stdin makes the daemon shut down gracefully; kill as a fallback.
    try {
      await _process.stdin.close();
    } catch (_) {}
    final exited = await _process.exitCode
        .timeout(const Duration(seconds: 3), onTimeout: () => -1);
    if (exited == -1) _process.kill();
  }
}

/// Locate the daemon binary: $MICYOU_DAEMON → next to this executable →
/// platform bundle-relative spots → bare name (PATH).
String daemonExecutable() {
  final exeName =
      Platform.isWindows ? 'micyou-daemon.exe' : 'micyou-daemon';
  final fromEnv = Platform.environment['MICYOU_DAEMON'];
  if (fromEnv != null && fromEnv.isNotEmpty && File(fromEnv).existsSync()) {
    return fromEnv;
  }
  final exeDir = File(Platform.resolvedExecutable).parent;
  final candidates = <String>[
    '${exeDir.path}${Platform.pathSeparator}$exeName',
    // linux: zip root holds the daemon, bundle lives in app/
    '${exeDir.parent.path}${Platform.pathSeparator}$exeName',
    // macOS .app: Contents/MacOS → Contents → MyApp.app → containing dir
    '${exeDir.parent.parent.parent.path}${Platform.pathSeparator}$exeName',
  ];
  for (final c in candidates) {
    if (File(c).existsSync()) return c;
  }
  return exeName; // resolved through PATH by Process.start
}

class BackendClient {
  BackendClient(this._transport) {
    _transport.lines.listen(
      _onMessage,
      onDone: () {
        _connected = false;
        _failAll(RpcError(-1, '连接已关闭'));
        _events.close();
      },
      onError: (Object e) {
        _connected = false;
        _failAll(RpcError(-1, '$e'));
      },
    );
    _connected = true;
  }

  final JsonTransport _transport;
  final Map<int, Completer<dynamic>> _pending = <int, Completer<dynamic>>{};
  final StreamController<Map<String, dynamic>> _events =
      StreamController<Map<String, dynamic>>.broadcast();
  int _nextId = 1;
  bool _connected = false;

  bool get connected => _connected;
  Stream<Map<String, dynamic>> get events => _events.stream;

  /// Connect to a remote daemon over WebSocket (`ws://host:port/rpc`).
  static Future<BackendClient> connectWs(String url) async {
    final channel = WebSocketChannel.connect(Uri.parse(url));
    await channel.ready;
    return BackendClient(WsTransport(channel));
  }

  /// Spawn the local daemon as a sidecar child (stdio JSON-RPC), like the
  /// real MicYou desktop app does. The child is closed when the client is.
  static Future<BackendClient> connectSidecar() async {
    final program = daemonExecutable();
    final process = await Process.start(
      program,
      <String>['--stdio', '--no-mode-lock'],
      mode: ProcessStartMode.normal,
    );
    return BackendClient(StdioTransport(process));
  }

  Future<dynamic> call(String method, [Map<String, dynamic>? params]) {
    final id = _nextId++;
    final completer = Completer<dynamic>();
    _pending[id] = completer;
    _transport.send(jsonEncode(<String, dynamic>{
      'jsonrpc': '2.0',
      'id': id,
      'method': method,
      'params': params ?? <String, dynamic>{},
    }));
    return completer.future.timeout(
      const Duration(seconds: 90),
      onTimeout: () {
        _pending.remove(id);
        throw RpcError(-32005, '$method 超时');
      },
    );
  }

  void _onMessage(dynamic raw) {
    Map<String, dynamic> msg;
    try {
      msg = jsonDecode(raw as String) as Map<String, dynamic>;
    } catch (_) {
      return;
    }
    if (!msg.containsKey('id') || msg['id'] == null) {
      if (msg['method'] == 'event') {
        final params = Map<String, dynamic>.from(msg['params'] as Map);
        _events.add(params);
      }
      return;
    }
    final id = (msg['id'] as num).toInt();
    final completer = _pending.remove(id);
    if (completer == null) return;
    if (msg.containsKey('error') && msg['error'] != null) {
      final err = Map<String, dynamic>.from(msg['error'] as Map);
      completer.completeError(RpcError(
        (err['code'] as num?)?.toInt() ?? -32603,
        err['message']?.toString() ?? '未知错误',
      ));
    } else {
      completer.complete(msg['result']);
    }
  }

  void _failAll(Object error) {
    final pending = List<Completer<dynamic>>.from(_pending.values);
    _pending.clear();
    for (final c in pending) {
      if (!c.isCompleted) c.completeError(error);
    }
  }

  Future<void> close() async {
    await _transport.close();
  }
}

/* ══════════════════════ app state ══════════════════════ */

class LogEntry {
  LogEntry(this.time, this.text);
  final DateTime time;
  final String text;
}

class AppState extends ChangeNotifier {
  AppState();

  BackendClient? client;
  Map<String, dynamic> hello = <String, dynamic>{};
  bool busy = false;

  // live state
  Map<String, dynamic> status = <String, dynamic>{};
  int level = 0;
  List<double> spectrumRaw = <double>[];
  List<double> spectrumProc = <double>[];
  final List<LogEntry> log = <LogEntry>[];

  // audio
  Map<String, dynamic> dsp = <String, dynamic>{};
  List<String> devices = <String>[];

  // connection
  List<String> ips = <String>[];
  int netPort = 0;
  List<Map<String, dynamic>> ifaces = <Map<String, dynamic>>[];
  List<Map<String, dynamic>> usbDevices = <Map<String, dynamic>>[];
  Map<String, dynamic> webStatus = <String, dynamic>{};
  Map<String, dynamic> prefs = <String, dynamic>{};

  // devices
  Map<String, dynamic> vbc = <String, dynamic>{};
  Map<String, dynamic> bh = <String, dynamic>{};
  Map<String, dynamic> pw = <String, dynamic>{};
  final List<String> installProgress = <String>[];

  // plugins
  List<Map<String, dynamic>> plugins = <Map<String, dynamic>>[];
  Map<String, dynamic> pluginSync = <String, dynamic>{};

  // settings/system
  Map<String, dynamic> uiPrefs = <String, dynamic>{};
  Map<String, dynamic> themeColors = <String, dynamic>{};
  Map<String, dynamic> version = <String, dynamic>{};
  Map<String, dynamic> modeStatus = <String, dynamic>{};
  Map<String, dynamic>? updateResult;
  String logPath = '';
  String logTail = '';

  String get os => hello['os']?.toString() ?? '';

  void addLog(String text) {
    log.insert(0, LogEntry(DateTime.now(), text));
    if (log.length > 200) log.removeLast();
    notifyListeners();
  }

  Future<void> connectSidecar() async {
    await _finishConnect(await BackendClient.connectSidecar(), '本地守护进程（stdio sidecar）');
  }

  Future<void> connectWs(String url) async {
    await _finishConnect(await BackendClient.connectWs(url), url);
  }

  Future<void> _finishConnect(BackendClient c, String label) async {
    client = c;
    hello = Map<String, dynamic>.from(
        await c.call('session/hello', <String, dynamic>{'name': 'flutter-frontend', 'ui': true}) as Map);
    await c.call('session/subscribe', <String, dynamic>{
      'events': <String>['*'],
    });
    c.events.listen(_onEvent);
    addLog('已连接[$label]: ${hello['backend']} ${hello['version']} (api v${hello['apiVersion']}, $os)');
    await refreshStatus();
    await loadPrefs();
    notifyListeners();
  }

  Future<dynamic> call(String method, [Map<String, dynamic>? params]) async {
    final c = client;
    if (c == null) throw RpcError(-1, '未连接');
    return c.call(method, params);
  }

  Future<void> refreshStatus() async {
    try {
      status = Map<String, dynamic>.from(await call('server/status') as Map);
      notifyListeners();
    } catch (e) {
      addLog('✗ 状态: $e');
    }
  }

  Future<void> loadPrefs() async {
    try {
      prefs = Map<String, dynamic>.from(await call('server/prefs/get') as Map);
      uiPrefs = Map<String, dynamic>.from(await call('config/ui/get') as Map);
      themeColors = Map<String, dynamic>.from(await call('config/theme/get') as Map);
      notifyListeners();
    } catch (_) {}
  }

  Future<void> loadAudio() async {
    try {
      dsp = Map<String, dynamic>.from(await call('audio/settings/get') as Map);
      devices = (await call('audio/devices') as List).map((e) => e.toString()).toList();
      notifyListeners();
    } catch (e) {
      addLog('✗ 音频设置: $e');
    }
  }

  Future<void> loadConnection() async {
    try {
      final info = Map<String, dynamic>.from(await call('network/info') as Map);
      ips = (info['ips'] as List? ?? <dynamic>[]).map((e) => e.toString()).toList();
      netPort = (info['port'] as num?)?.toInt() ?? 0;
      ifaces = (await call('network/interfaces') as List)
          .map((e) => Map<String, dynamic>.from(e as Map))
          .toList();
      try {
        usbDevices = (await call('usb/devices') as List)
            .map((e) => Map<String, dynamic>.from(e as Map))
            .toList();
      } catch (_) {
        usbDevices = <Map<String, dynamic>>[];
      }
      webStatus = Map<String, dynamic>.from(await call('web/status') as Map);
      notifyListeners();
    } catch (e) {
      addLog('✗ 连接信息: $e');
    }
  }

  Future<void> loadDevices() async {
    try {
      if (os == 'windows') {
        vbc = Map<String, dynamic>.from(await call('devices/vbcable/check') as Map);
      } else if (os == 'macos') {
        bh = Map<String, dynamic>.from(await call('devices/blackhole/check') as Map);
      } else if (os == 'linux') {
        pw = Map<String, dynamic>.from(await call('devices/pipewire/check') as Map);
      }
      notifyListeners();
    } catch (e) {
      addLog('✗ 设备状态: $e');
    }
  }

  Future<void> loadPlugins() async {
    try {
      plugins = (await call('plugins/list') as List)
          .map((e) => Map<String, dynamic>.from(e as Map))
          .toList();
      pluginSync = Map<String, dynamic>.from(await call('plugins/syncStatus') as Map);
      notifyListeners();
    } catch (e) {
      addLog('✗ 插件: $e');
    }
  }

  Future<void> loadSystem() async {
    try {
      version = Map<String, dynamic>.from(await call('system/version') as Map);
      modeStatus = Map<String, dynamic>.from(await call('mode/status') as Map);
      final p = Map<String, dynamic>.from(await call('system/log/path') as Map);
      logPath = p['path']?.toString() ?? '';
      notifyListeners();
    } catch (e) {
      addLog('✗ 系统: $e');
    }
  }

  void _onEvent(Map<String, dynamic> ev) {
    final type = ev['type']?.toString() ?? '';
    final data = Map<String, dynamic>.from(ev['data'] as Map? ?? <String, dynamic>{});
    switch (type) {
      case 'audioLevel':
        level = (data['level'] as num?)?.toInt() ?? 0;
        notifyListeners();
        break;
      case 'audioSpectrum':
        spectrumRaw = (data['raw'] as List? ?? <dynamic>[]).map((e) => (e as num).toDouble()).toList();
        spectrumProc =
            (data['processed'] as List? ?? <dynamic>[]).map((e) => (e as num).toDouble()).toList();
        notifyListeners();
        break;
      case 'muteStateChanged':
        addLog('静音 → ${data['muted']}');
        refreshStatus();
        break;
      case 'monitoringChanged':
        addLog('监听 → ${data['enabled']}');
        refreshStatus();
        break;
      case 'deviceConnected':
        final dev = Map<String, dynamic>.from(data['device'] as Map? ?? <String, dynamic>{});
        addLog('📱 设备连接: ${dev['name']} (${dev['ip']})');
        refreshStatus();
        break;
      case 'deviceDisconnected':
        addLog('设备断开');
        level = 0;
        refreshStatus();
        break;
      case 'serverStopped':
        addLog('服务器已停止');
        level = 0;
        refreshStatus();
        break;
      case 'audioMetrics':
        final m = Map<String, dynamic>.from(data['metrics'] as Map? ?? <String, dynamic>{});
        addLog('指标 延迟${m['latencyMs']}ms 网络${m['networkLatencyMs']}ms '
            '抖动${m['jitterMs']}ms 丢包${m['packetLossRate']}%');
        break;
      case 'udpAudioWarning':
        addLog('⚠ 长时间未收到 UDP 音频（防火墙？）');
        break;
      case 'aecStatusChanged':
        addLog('AEC 状态: ${data['status']}');
        break;
      case 'installProgress':
        installProgress.add(data['message']?.toString() ?? '');
        notifyListeners();
        break;
      case 'pluginListChanged':
        addLog('插件变更: ${data['pluginId']}');
        loadPlugins();
        break;
      case 'pluginLog':
        addLog('插件[${data['pluginId']}] ${data['level']}: ${data['message']}');
        break;
      case 'pluginDownloadProgress':
        addLog('下载 ${data['id']}: ${data['downloaded']}/${data['total']}'
            '${data['done'] == true ? ' ✔' : ''}');
        break;
      case 'uiRequest':
        addLog('UI 请求（本前端未实现插件面板窗口）: ${data['request']}');
        break;
      default:
        addLog('事件 $type');
    }
  }
}

/* ══════════════════════ app shell ══════════════════════ */

class MicYouApp extends StatelessWidget {
  const MicYouApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'MicYou · Flutter',
      debugShowCheckedModeBanner: false,
      themeMode: ThemeMode.dark,
      darkTheme: ThemeData(
        useMaterial3: true,
        colorScheme: ColorScheme.fromSeed(
          seedColor: const Color(0xFF5B7CFA),
          brightness: Brightness.dark,
        ),
      ),
      home: const ConnectPage(),
    );
  }
}

const String defaultWsUrl = 'ws://127.0.0.1:9610/rpc';

class ConnectPage extends StatefulWidget {
  const ConnectPage({super.key});

  @override
  State<ConnectPage> createState() => _ConnectPageState();
}

class _ConnectPageState extends State<ConnectPage> {
  final TextEditingController _url = TextEditingController(text: defaultWsUrl);
  final AppState _state = AppState();
  bool _connecting = false;
  String? _error;

  Future<void> _connectWs() async {
    setState(() {
      _connecting = true;
      _error = null;
    });
    try {
      await _state.connectWs(_url.text.trim());
      _enter();
    } catch (e) {
      setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _connecting = false);
    }
  }

  Future<void> _connectSidecar() async {
    setState(() {
      _connecting = true;
      _error = null;
    });
    try {
      await _state.connectSidecar();
      _enter();
    } catch (e) {
      setState(() => _error = '无法启动本地守护进程: $e\n'
          '（确认 micyou-daemon 与应用同目录、在 PATH 中，或用 MICYOU_DAEMON 环境变量指定）');
    } finally {
      if (mounted) setState(() => _connecting = false);
    }
  }

  void _enter() {
    if (!mounted) return;
    Navigator.of(context).pushReplacement(MaterialPageRoute<void>(
      builder: (_) => HomePage(state: _state),
    ));
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 460),
          child: Card(
            margin: const EdgeInsets.all(24),
            child: Padding(
              padding: const EdgeInsets.all(28),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: <Widget>[
                  Icon(Icons.podcasts, size: 52, color: scheme.primary),
                  const SizedBox(height: 12),
                  Text('MicYou · libmicyou',
                      textAlign: TextAlign.center,
                      style: Theme.of(context).textTheme.headlineSmall),
                  const SizedBox(height: 4),
                  Text('Flutter (Material 3) 桌面参考前端 — sidecar stdio / WebSocket JSON-RPC',
                      textAlign: TextAlign.center,
                      style: Theme.of(context).textTheme.bodySmall),
                  const SizedBox(height: 24),
                  FilledButton.icon(
                    onPressed: _connecting ? null : _connectSidecar,
                    icon: _connecting
                        ? const SizedBox(
                            width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                        : const Icon(Icons.desktop_windows),
                    label: const Text('启动本地守护进程（sidecar）'),
                  ),
                  const SizedBox(height: 8),
                  Text('或连接远程后端（daemon --ws 127.0.0.1:9610）：',
                      style: Theme.of(context).textTheme.bodySmall),
                  const SizedBox(height: 8),
                  TextField(
                    controller: _url,
                    decoration: const InputDecoration(
                      labelText: 'WebSocket 地址',
                      border: OutlineInputBorder(),
                      prefixIcon: Icon(Icons.link),
                      isDense: true,
                    ),
                    onSubmitted: (_) => _connectWs(),
                  ),
                  if (_error != null) ...<Widget>[
                    const SizedBox(height: 12),
                    Text(_error!, style: TextStyle(color: scheme.error)),
                  ],
                  const SizedBox(height: 20),
                  OutlinedButton.icon(
                    onPressed: _connecting ? null : _connectWs,
                    icon: const Icon(Icons.login),
                    label: const Text('连接 WebSocket'),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.state});
  final AppState state;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  int _page = 0;

  static const List<String> _titles = <String>[
    '仪表盘', '音频与 DSP', '连接', '虚拟设备', '插件', '设置', '系统',
  ];

  AppState get s => widget.state;

  @override
  void initState() {
    super.initState();
    s.addListener(_onState);
  }

  void _onState() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    s.removeListener(_onState);
    super.dispose();
  }

  void _go(int index) {
    setState(() => _page = index);
    switch (index) {
      case 0:
        s.refreshStatus();
        break;
      case 1:
        s.loadAudio();
        break;
      case 2:
        s.loadConnection();
        break;
      case 3:
        s.loadDevices();
        break;
      case 4:
        s.loadPlugins();
        break;
      case 5:
        s.loadPrefs();
        break;
      case 6:
        s.loadSystem();
        break;
    }
  }

  @override
  Widget build(BuildContext context) {
    final running = s.status['isServerRunning'] == true;
    final connected = s.status['isConnected'] == true;
    final muted = s.status['isMuted'] == true;
    final monitoring = s.status['isMonitoring'] == true;

    return Scaffold(
      appBar: AppBar(
        title: Text('MicYou · ${_titles[_page]}'),
        actions: <Widget>[
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 6),
            child: Chip(
              visualDensity: VisualDensity.compact,
              label: Text(s.status['phase']?.toString() ?? '-'),
              avatar: Icon(Icons.circle,
                  size: 10, color: running ? Colors.greenAccent : Colors.grey),
            ),
          ),
          Chip(
            visualDensity: VisualDensity.compact,
            label: Text(connected ? '设备已连接' : '无设备'),
            avatar: Icon(Icons.phone_android,
                size: 14, color: connected ? Colors.greenAccent : Colors.grey),
          ),
          const SizedBox(width: 8),
          IconButton(
            tooltip: '静音',
            icon: Icon(muted ? Icons.mic_off : Icons.mic),
            selectedIcon: Icon(Icons.mic_off, color: Theme.of(context).colorScheme.error),
            isSelected: muted,
            onPressed: () async {
              try {
                await s.call('audio/mute/set', <String, dynamic>{'muted': !muted});
              } catch (e) {
                s.addLog('✗ 静音: $e');
              }
            },
          ),
          IconButton(
            tooltip: '耳返监听',
            icon: Icon(Icons.headphones),
            isSelected: monitoring,
            onPressed: () async {
              try {
                await s.call('audio/monitoring/set', <String, dynamic>{'enabled': !monitoring});
              } catch (e) {
                s.addLog('✗ 监听: $e');
              }
            },
          ),
          const SizedBox(width: 10),
        ],
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(4),
          child: LinearProgressIndicator(
            value: (s.level / 100).clamp(0.0, 1.0),
            minHeight: 4,
            backgroundColor: Colors.transparent,
          ),
        ),
      ),
      body: Row(
        children: <Widget>[
          NavigationRail(
            selectedIndex: _page,
            onDestinationSelected: _go,
            labelType: NavigationRailLabelType.all,
            destinations: const <NavigationRailDestination>[
              NavigationRailDestination(icon: Icon(Icons.dashboard_outlined), selectedIcon: Icon(Icons.dashboard), label: Text('总览')),
              NavigationRailDestination(icon: Icon(Icons.tune), label: Text('音频')),
              NavigationRailDestination(icon: Icon(Icons.wifi_tethering), label: Text('连接')),
              NavigationRailDestination(icon: Icon(Icons.mic_external_on), label: Text('设备')),
              NavigationRailDestination(icon: Icon(Icons.extension_outlined), label: Text('插件')),
              NavigationRailDestination(icon: Icon(Icons.settings_outlined), label: Text('设置')),
              NavigationRailDestination(icon: Icon(Icons.info_outline), label: Text('系统')),
            ],
          ),
          const VerticalDivider(width: 1),
          Expanded(
            child: IndexedStack(
              index: _page,
              children: <Widget>[
                DashboardPage(state: s),
                AudioPage(state: s),
                ConnectionPage(state: s),
                DevicesPage(state: s),
                PluginsPage(state: s),
                SettingsPage(state: s),
                SystemPage(state: s),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/* ══════════════════════ shared bits ══════════════════════ */

class PagePad extends StatelessWidget {
  const PagePad({super.key, required this.child});
  final Widget child;
  @override
  Widget build(BuildContext context) {
    return SingleChildScrollView(
      padding: const EdgeInsets.all(16),
      child: ConstrainedBox(constraints: const BoxConstraints(maxWidth: 1000), child: child),
    );
  }
}

Future<void> guard(AppState s, String what, Future<void> Function() action) async {
  try {
    await action();
  } catch (e) {
    s.addLog('✗ $what: $e');
  }
}

class LabeledSlider extends StatelessWidget {
  const LabeledSlider({
    super.key,
    required this.label,
    required this.value,
    required this.min,
    required this.max,
    this.divisions,
    this.format,
    required this.onChanged,
  });
  final String label;
  final double value;
  final double min;
  final double max;
  final int? divisions;
  final String Function(double)? format;
  final ValueChanged<double> onChanged;

  @override
  Widget build(BuildContext context) {
    final text = format != null ? format!(value) : value.toStringAsFixed(1);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        children: <Widget>[
          SizedBox(width: 110, child: Text(label, style: Theme.of(context).textTheme.bodyMedium)),
          Expanded(
            child: Slider(
              value: value.clamp(min, max),
              min: min,
              max: max,
              divisions: divisions,
              label: text,
              onChanged: onChanged,
            ),
          ),
          SizedBox(width: 64, child: Text(text, textAlign: TextAlign.right)),
        ],
      ),
    );
  }
}

class SpectrumPainter extends CustomPainter {
  SpectrumPainter(this.raw, this.processed);
  final List<double> raw;
  final List<double> processed;

  void _draw(Canvas canvas, Size size, List<double> data, Color color) {
    if (data.isEmpty) return;
    final paint = Paint()..color = color;
    final bw = size.width / data.length;
    for (int i = 0; i < data.length; i++) {
      final v = data[i].clamp(0.0, 1.0);
      final h = v * size.height;
      canvas.drawRect(
        Rect.fromLTWH(i * bw + 0.5, size.height - h, math.max(1.0, bw - 1.5), h),
        paint,
      );
    }
  }

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(Offset.zero & size, Paint()..color = const Color(0xFF10131A));
    _draw(canvas, size, raw, const Color(0x555B7CFA));
    _draw(canvas, size, processed, const Color(0xCC3ECF8E));
  }

  @override
  bool shouldRepaint(SpectrumPainter old) => true;
}

/* ══════════════════════ dashboard ══════════════════════ */

class DashboardPage extends StatefulWidget {
  const DashboardPage({super.key, required this.state});
  final AppState state;

  @override
  State<DashboardPage> createState() => _DashboardPageState();
}

class _DashboardPageState extends State<DashboardPage> {
  late final TextEditingController _port = TextEditingController(
      text: '${widget.state.prefs['port'] ?? 18554}');
  String _mode = 'wifi';
  bool _spectrum = false;

  @override
  Widget build(BuildContext context) {
    final s = widget.state;
    if (s.prefs['mode'] != null && _mode == 'wifi') _mode = s.prefs['mode'].toString();
    final running = s.status['isServerRunning'] == true;
    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('服务器', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 12),
                  Wrap(
                    spacing: 12,
                    runSpacing: 12,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    children: <Widget>[
                      SizedBox(
                        width: 120,
                        child: TextField(
                          controller: _port,
                          decoration: const InputDecoration(labelText: '端口', isDense: true, border: OutlineInputBorder()),
                        ),
                      ),
                      SegmentedButton<String>(
                        segments: const <ButtonSegment<String>>[
                          ButtonSegment(value: 'wifi', label: Text('Wi-Fi'), icon: Icon(Icons.wifi)),
                          ButtonSegment(value: 'usb', label: Text('USB'), icon: Icon(Icons.usb)),
                          ButtonSegment(value: 'web', label: Text('Web'), icon: Icon(Icons.language)),
                        ],
                        selected: <String>{_mode},
                        onSelectionChanged: (Set<String> v) => setState(() => _mode = v.first),
                      ),
                      FilledButton.icon(
                        onPressed: running
                            ? null
                            : () => guard(s, '启动', () async {
                                  final r = await s.call('server/start', <String, dynamic>{
                                    'port': int.tryParse(_port.text) ?? 18554,
                                    'mode': _mode,
                                  }) as Map;
                                  s.addLog('✓ ${r['message']}');
                                  await s.refreshStatus();
                                }),
                        icon: const Icon(Icons.play_arrow),
                        label: const Text('启动'),
                      ),
                      OutlinedButton.icon(
                        onPressed: !running
                            ? null
                            : () => guard(s, '停止', () async {
                                  final r = await s.call('server/stop') as Map;
                                  s.addLog('✓ ${r['message']}');
                                  await s.refreshStatus();
                                }),
                        icon: const Icon(Icons.stop),
                        label: const Text('停止'),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: <Widget>[
                  Row(
                    children: <Widget>[
                      Text('电平 / 频谱', style: Theme.of(context).textTheme.titleMedium),
                      const Spacer(),
                      Text('${s.level}%', style: Theme.of(context).textTheme.titleLarge),
                    ],
                  ),
                  const SizedBox(height: 8),
                  ClipRRect(
                    borderRadius: BorderRadius.circular(8),
                    child: LinearProgressIndicator(
                      value: (s.level / 100).clamp(0.0, 1.0),
                      minHeight: 18,
                    ),
                  ),
                  const SizedBox(height: 10),
                  SizedBox(
                    height: 110,
                    child: CustomPaint(
                      painter: SpectrumPainter(s.spectrumRaw, s.spectrumProc),
                      size: Size.infinite,
                    ),
                  ),
                  Align(
                    alignment: Alignment.centerLeft,
                    child: SwitchListTile(
                      contentPadding: EdgeInsets.zero,
                      title: const Text('频谱流（audio/spectrum）'),
                      dense: true,
                      value: _spectrum,
                      onChanged: (v) {
                        setState(() => _spectrum = v);
                        guard(s, '频谱', () async {
                          await s.call('audio/spectrum/set', <String, dynamic>{'enabled': v});
                        });
                      },
                    ),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('事件日志', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 8),
                  SizedBox(
                    height: 240,
                    child: ListView(
                      children: s.log
                          .map((e) => Text(
                              '[${e.time.hour.toString().padLeft(2, '0')}:'
                              '${e.time.minute.toString().padLeft(2, '0')}:'
                              '${e.time.second.toString().padLeft(2, '0')}] ${e.text}',
                              style: Theme.of(context).textTheme.bodySmall))
                          .toList(),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/* ══════════════════════ audio & dsp ══════════════════════ */

class AudioPage extends StatefulWidget {
  const AudioPage({super.key, required this.state});
  final AppState state;

  @override
  State<AudioPage> createState() => _AudioPageState();
}

class _AudioPageState extends State<AudioPage> {
  Map<String, dynamic> _form = <String, dynamic>{};
  String? _device;

  double _d(String key, double def) => ((_form[key] ?? def) as num).toDouble();
  bool _b(String key) => _form[key] == true;

  void _sync() {
    final s = widget.state;
    _form = Map<String, dynamic>.from(s.dsp);
    final eq = Map<String, dynamic>.from(_form['equalizer'] as Map? ?? <String, dynamic>{});
    _form['equalizer'] = eq;
    final current = s.prefs['outputDevice']?.toString() ?? '';
    _device = (current.isEmpty || current == 'auto' || current == 'default') ? null : current;
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.state;
    if (_form.isEmpty && s.dsp.isNotEmpty) _sync();
    final eq = Map<String, dynamic>.from(_form['equalizer'] as Map? ?? <String, dynamic>{});
    final gains = ((eq['gains'] as List?) ?? List<double>.filled(10, 0))
        .map((e) => (e as num).toDouble())
        .toList();
    final chain = ((_form['processingChain'] as List?) ?? <dynamic>[]).map((e) => e.toString()).toList();

    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('输出', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 8),
                  DropdownButtonFormField<String>(
                    value: _device != null && s.devices.contains(_device) ? _device : null,
                    isExpanded: true,
                    decoration: const InputDecoration(labelText: '输出设备（下次启动生效）', border: OutlineInputBorder()),
                    items: <DropdownMenuItem<String>>[
                      const DropdownMenuItem(value: null, child: Text('（默认 / 虚拟设备）')),
                      ...s.devices.map((d) => DropdownMenuItem(value: d, child: Text(d))),
                    ],
                    onChanged: (v) => guard(s, '输出设备', () async {
                      final p = Map<String, dynamic>.from(s.prefs);
                      p['outputDevice'] = v ?? '';
                      await s.call('server/prefs/save', p);
                      s.addLog('✓ 输出设备已保存');
                      await s.loadPrefs();
                      setState(() => _device = v);
                    }),
                  ),
                  const SizedBox(height: 10),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('硬静音'),
                    value: s.status['isMuted'] == true,
                    onChanged: (v) => guard(s, '静音', () async {
                      await s.call('audio/mute/set', <String, dynamic>{'muted': v});
                    }),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('耳返监听'),
                    value: s.status['isMonitoring'] == true,
                    onChanged: (v) => guard(s, '监听', () async {
                      await s.call('audio/monitoring/set', <String, dynamic>{'enabled': v});
                    }),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('回声消除 AEC'),
                    subtitle: s.os == 'macos' ? const Text('macOS 不支持') : null,
                    value: _b('aecEnabled'),
                    onChanged: s.os == 'macos'
                        ? null
                        : (v) => setState(() => _form['aecEnabled'] = v),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('DSP 处理链', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 8),
                  Wrap(
                    spacing: 6,
                    runSpacing: 6,
                    children: chain
                        .map((n) => Chip(
                              label: Text(n),
                              visualDensity: VisualDensity.compact,
                              backgroundColor: n.startsWith('Plugin:')
                                  ? Theme.of(context).colorScheme.primaryContainer
                                  : null,
                            ))
                        .toList(),
                  ),
                  const Divider(height: 24),
                  LabeledSlider(
                    label: '增益 (dB)',
                    value: _d('gain', 0),
                    min: -50,
                    max: 50,
                    divisions: 200,
                    onChanged: (v) => setState(() => _form['gain'] = v),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('降噪 NS'),
                    value: _b('nsEnabled'),
                    onChanged: (v) => setState(() => _form['nsEnabled'] = v),
                  ),
                  DropdownButtonFormField<String>(
                    value: (_form['nsType'] ?? 'PureVox').toString(),
                    decoration: const InputDecoration(labelText: 'NS 类型', isDense: true, border: OutlineInputBorder()),
                    items: const <DropdownMenuItem<String>>[
                      DropdownMenuItem(value: 'PureVox', child: Text('PureVox (AI)')),
                      DropdownMenuItem(value: 'RNNoise', child: Text('RNNoise')),
                      DropdownMenuItem(value: 'Speexdsp', child: Text('Speexdsp')),
                    ],
                    onChanged: (v) => setState(() => _form['nsType'] = v),
                  ),
                  const SizedBox(height: 8),
                  LabeledSlider(
                    label: 'NS 强度',
                    value: _d('nsIntensity', 50),
                    min: 0,
                    max: 100,
                    divisions: 100,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['nsIntensity'] = v),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('去混响'),
                    value: _b('dereverbEnabled'),
                    onChanged: (v) => setState(() => _form['dereverbEnabled'] = v),
                  ),
                  LabeledSlider(
                    label: '去混响强度',
                    value: _d('dereverbLevel', 50),
                    min: 0,
                    max: 100,
                    divisions: 100,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['dereverbLevel'] = v),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('自动增益 AGC'),
                    value: _b('agcEnabled'),
                    onChanged: (v) => setState(() => _form['agcEnabled'] = v),
                  ),
                  LabeledSlider(
                    label: 'AGC 目标',
                    value: _d('agcTarget', 16000),
                    min: 0,
                    max: 32767,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['agcTarget'] = v),
                  ),
                  LabeledSlider(
                    label: 'Attack',
                    value: _d('agcAttack', 50),
                    min: 1,
                    max: 100,
                    divisions: 99,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['agcAttack'] = v),
                  ),
                  LabeledSlider(
                    label: 'Decay',
                    value: _d('agcDecay', 50),
                    min: 1,
                    max: 100,
                    divisions: 99,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['agcDecay'] = v),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('语音门限 VAD'),
                    value: _b('vadEnabled'),
                    onChanged: (v) => setState(() => _form['vadEnabled'] = v),
                  ),
                  LabeledSlider(
                    label: 'VAD 阈值 (dB)',
                    value: _d('vadThreshold', -40),
                    min: -100,
                    max: 0,
                    divisions: 100,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['vadThreshold'] = v),
                  ),
                  LabeledSlider(
                    label: '输出缓冲 (ms)',
                    value: _d('outputBufferMs', 300),
                    min: 100,
                    max: 1200,
                    divisions: 22,
                    format: (v) => v.toStringAsFixed(0),
                    onChanged: (v) => setState(() => _form['outputBufferMs'] = v.round()),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('均衡器 EQ（10 段）'),
                    value: eq['enabled'] == true,
                    onChanged: (v) => setState(() => eq['enabled'] = v),
                  ),
                  LabeledSlider(
                    label: '前置放大',
                    value: ((eq['preAmp'] ?? 0) as num).toDouble(),
                    min: -12,
                    max: 12,
                    divisions: 48,
                    onChanged: (v) => setState(() => eq['preAmp'] = v),
                  ),
                  const SizedBox(height: 6),
                  ...List<Widget>.generate(10, (i) {
                    const labels = <String>['31Hz', '62Hz', '125Hz', '250Hz', '500Hz', '1kHz', '2kHz', '4kHz', '8kHz', '16kHz'];
                    final v = (gains.length > i ? gains[i] : 0.0);
                    return LabeledSlider(
                      label: labels[i],
                      value: v,
                      min: -12,
                      max: 12,
                      divisions: 48,
                      onChanged: (nv) => setState(() {
                        final g = List<double>.from(gains);
                        while (g.length < 10) {
                          g.add(0);
                        }
                        g[i] = nv;
                        eq['gains'] = g;
                      }),
                    );
                  }),
                  const Divider(height: 24),
                  Row(
                    children: <Widget>[
                      FilledButton.icon(
                        icon: const Icon(Icons.save),
                        label: const Text('保存并应用'),
                        onPressed: () => guard(s, '保存 DSP', () async {
                          final payload = Map<String, dynamic>.from(_form);
                          payload['equalizer'] = eq;
                          await s.call('audio/settings/update', <String, dynamic>{'settings': payload});
                          s.addLog('✓ DSP 已保存并热应用');
                          await s.loadAudio();
                          setState(() => _sync());
                        }),
                      ),
                      const SizedBox(width: 10),
                      OutlinedButton.icon(
                        icon: const Icon(Icons.refresh),
                        label: const Text('重载'),
                        onPressed: () async {
                          await s.loadAudio();
                          setState(() => _sync());
                        },
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/* ══════════════════════ connection ══════════════════════ */

class ConnectionPage extends StatefulWidget {
  const ConnectionPage({super.key, required this.state});
  final AppState state;

  @override
  State<ConnectionPage> createState() => _ConnectionPageState();
}

class _ConnectionPageState extends State<ConnectionPage> {
  String? _usbSerial;

  String _host(String ip) => ip.contains(':') ? '[$ip]' : ip;

  @override
  Widget build(BuildContext context) {
    final s = widget.state;
    final webPort = (s.prefs['webPort'] as num?)?.toInt() ?? 8443;
    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('局域网地址（手机连接 / 扫码内容）', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 10),
                  Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    children: s.ips
                        .map((ip) => Chip(
                              avatar: const Icon(Icons.lan_outlined, size: 16),
                              label: Text('$ip:${s.netPort}'),
                            ))
                        .toList(),
                  ),
                  const SizedBox(height: 10),
                  if (s.os == 'windows')
                    OutlinedButton.icon(
                      icon: const Icon(Icons.shield),
                      label: const Text('允许防火墙入站（UAC）'),
                      onPressed: () => guard(s, '防火墙', () async {
                        await s.call('network/firewall/allow');
                        s.addLog('✓ 已请求防火墙规则');
                      }),
                    ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('网络接口', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 8),
                  DataTable(
                    columnSpacing: 32,
                    columns: const <DataColumn>[
                      DataColumn(label: Text('IP')),
                      DataColumn(label: Text('接口')),
                    ],
                    rows: s.ifaces
                        .map((i) => DataRow(cells: <DataCell>[
                              DataCell(Text(i['ip']?.toString() ?? '')),
                              DataCell(Text(i['interfaceName']?.toString() ?? '')),
                            ]))
                        .toList(),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('USB（adb reverse）', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 10),
                  Row(
                    children: <Widget>[
                      Expanded(
                        child: DropdownButtonFormField<String>(
                          value: _usbSerial,
                          isExpanded: true,
                          decoration: const InputDecoration(border: OutlineInputBorder(), isDense: true),
                          items: <DropdownMenuItem<String>>[
                            const DropdownMenuItem(value: null, child: Text('（自动选择）')),
                            ...s.usbDevices.map((d) => DropdownMenuItem(
                                  value: d['serial']?.toString(),
                                  child: Text('${d['name']} [${d['serial']}]'),
                                )),
                          ],
                          onChanged: (v) => setState(() => _usbSerial = v),
                        ),
                      ),
                      const SizedBox(width: 10),
                      IconButton.filledTonal(
                        tooltip: '刷新',
                        icon: const Icon(Icons.refresh),
                        onPressed: () => s.loadConnection(),
                      ),
                      const SizedBox(width: 10),
                      FilledButton(
                        onPressed: () => guard(s, 'USB', () async {
                          final r = await s.call('usb/enable', <String, dynamic>{
                            if (_usbSerial != null) 'deviceSerial': _usbSerial,
                          });
                          s.addLog('✓ USB: ${jsonEncode(r)}');
                        }),
                        child: const Text('启用'),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('Web 模式', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 6),
                  Text('运行: ${s.webStatus['running'] == true ? '是' : '否'} · '
                      '浏览器客户端: ${s.webStatus['clientCount'] ?? 0}'),
                  const SizedBox(height: 8),
                  SelectableText(
                    s.ips.map((ip) => 'https://${_host(ip)}:$webPort').join('\n'),
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/* ══════════════════════ devices ══════════════════════ */

class DevicesPage extends StatelessWidget {
  const DevicesPage({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    final s = state;
    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          if (s.os == 'windows')
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: <Widget>[
                    Text('VB-CABLE（Windows）', style: Theme.of(context).textTheme.titleMedium),
                    const SizedBox(height: 6),
                    Text('已安装: ${s.vbc['installed'] == true ? '✅ 是' : '❌ 否'}'),
                    const SizedBox(height: 10),
                    Row(
                      children: <Widget>[
                        OutlinedButton(
                          onPressed: () => s.loadDevices(),
                          child: const Text('检查'),
                        ),
                        const SizedBox(width: 10),
                        FilledButton(
                          onPressed: () => guard(s, 'VB-CABLE 安装', () async {
                            final r = await s.call('devices/vbcable/install') as Map;
                            s.addLog('VB-CABLE: ${r['success'] == true ? '✅ 成功' : '❌ ${r['message']}'}');
                            await s.loadDevices();
                          }),
                          child: const Text('下载并安装（UAC）'),
                        ),
                      ],
                    ),
                    if (s.installProgress.isNotEmpty) ...<Widget>[
                      const SizedBox(height: 10),
                      SizedBox(
                        height: 100,
                        child: ListView(
                          children: s.installProgress
                              .map((m) => Text(m, style: Theme.of(context).textTheme.bodySmall))
                              .toList(),
                        ),
                      ),
                    ],
                  ],
                ),
              ),
            ),
          if (s.os == 'macos')
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: <Widget>[
                    Text('BlackHole（macOS）', style: Theme.of(context).textTheme.titleMedium),
                    const SizedBox(height: 6),
                    SelectableText(jsonEncode(s.bh)),
                    const SizedBox(height: 10),
                    Wrap(
                      spacing: 10,
                      children: <Widget>[
                        OutlinedButton(onPressed: () => s.loadDevices(), child: const Text('检查')),
                        FilledButton(
                          onPressed: () => guard(s, 'BlackHole', () async {
                            await s.call('devices/blackhole/setInput');
                            s.addLog('✓ 已设为系统输入');
                            await s.loadDevices();
                          }),
                          child: const Text('设为系统输入'),
                        ),
                        OutlinedButton(
                          onPressed: () => guard(s, 'BlackHole', () async {
                            await s.call('devices/blackhole/restore');
                            s.addLog('✓ 已恢复原输入');
                            await s.loadDevices();
                          }),
                          child: const Text('恢复'),
                        ),
                      ],
                    ),
                  ],
                ),
              ),
            ),
          if (s.os == 'linux')
            Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: <Widget>[
                    Text('PipeWire（Linux）', style: Theme.of(context).textTheme.titleMedium),
                    const SizedBox(height: 6),
                    Text('可用: ${s.pw['available'] == true ? '是' : '否'} · '
                        '已建立: ${s.pw['setup'] == true ? '是' : '否'} · '
                        '设备存在: ${s.pw['deviceExists'] == true ? '是' : '否'} · '
                        '发行版: ${s.pw['distro'] ?? '-'}'),
                    if (s.pw['available'] != true && (s.pw['installCommand'] ?? '').toString().isNotEmpty)
                      Padding(
                        padding: const EdgeInsets.only(top: 6),
                        child: SelectableText('安装: ${s.pw['installCommand']}',
                            style: Theme.of(context).textTheme.bodySmall),
                      ),
                    const SizedBox(height: 10),
                    OutlinedButton(onPressed: () => s.loadDevices(), child: const Text('刷新状态')),
                    const SizedBox(height: 6),
                    Text('服务器启动时自动创建 MicYouVirtualSink/Source。',
                        style: Theme.of(context).textTheme.bodySmall),
                  ],
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/* ══════════════════════ plugins ══════════════════════ */

class PluginsPage extends StatelessWidget {
  const PluginsPage({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    final s = state;
    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Row(
            children: <Widget>[
              Text('插件', style: Theme.of(context).textTheme.titleMedium),
              const Spacer(),
              Text('跨设备: ${s.pluginSync['transportReady'] == true ? '就绪' : '未连接'}',
                  style: Theme.of(context).textTheme.bodySmall),
              const SizedBox(width: 10),
              IconButton.filledTonal(
                  icon: const Icon(Icons.refresh), onPressed: () => s.loadPlugins()),
              const SizedBox(width: 6),
              OutlinedButton.icon(
                icon: const Icon(Icons.folder_open),
                label: const Text('目录'),
                onPressed: () => guard(s, '插件目录', () async {
                  final r = await s.call('plugins/dir') as Map;
                  s.addLog('插件目录: ${r['path']}');
                }),
              ),
            ],
          ),
          const SizedBox(height: 8),
          if (s.plugins.isEmpty)
            const Card(
              child: Padding(
                padding: EdgeInsets.all(20),
                child: Text('暂无插件。将插件放入配置目录 plugins/ 后刷新。'),
              ),
            ),
          ...s.plugins.map((p) {
            final id = p['id']?.toString() ?? '';
            return Card(
              margin: const EdgeInsets.symmetric(vertical: 4),
              child: ListTile(
                leading: Icon(
                  p['runtime'] == 'wasm' ? Icons.memory : Icons.settings_input_component,
                ),
                title: Text('${p['name']}  (${p['runtime']} · ${p['kind']})'),
                subtitle: Text(
                  '$id · v${p['version']}'
                  '${p['dspNode'] == true ? ' · DSP 节点' : ''}'
                  '${(p['error'] ?? '').toString().isNotEmpty ? '\n⚠ ${p['error']}' : ''}',
                ),
                isThreeLine: (p['error'] ?? '').toString().isNotEmpty,
                trailing: Switch(
                  value: p['enabled'] == true,
                  onChanged: (v) => guard(s, '插件开关', () async {
                    await s.call('plugins/setEnabled', <String, dynamic>{'id': id, 'enabled': v});
                    await s.loadPlugins();
                  }),
                ),
                onTap: () => _openDetail(context, s, p),
              ),
            );
          }),
        ],
      ),
    );
  }

  void _openDetail(BuildContext context, AppState s, Map<String, dynamic> p) {
    final id = p['id']?.toString() ?? '';
    showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (BuildContext ctx) => PluginDetailSheet(state: s, pluginId: id, plugin: p),
    );
  }
}

class PluginDetailSheet extends StatefulWidget {
  const PluginDetailSheet({super.key, required this.state, required this.pluginId, required this.plugin});
  final AppState state;
  final String pluginId;
  final Map<String, dynamic> plugin;

  @override
  State<PluginDetailSheet> createState() => _PluginDetailSheetState();
}

class _PluginDetailSheetState extends State<PluginDetailSheet> {
  final TextEditingController _config = TextEditingController();
  final TextEditingController _action = TextEditingController();
  final TextEditingController _payload = TextEditingController();
  String _logs = '';

  @override
  void initState() {
    super.initState();
    _loadConfig();
    _loadLogs();
  }

  Future<void> _loadConfig() async {
    try {
      final cfg = await widget.state.call('plugins/config/get', <String, dynamic>{'id': widget.pluginId});
      _config.text = const JsonEncoder.withIndent('  ').convert(cfg);
      if (mounted) setState(() {});
    } catch (e) {
      _config.text = '// $e';
    }
  }

  Future<void> _loadLogs() async {
    try {
      final lines = await widget.state.call('plugins/logs', <String, dynamic>{'id': widget.pluginId}) as List;
      _logs = lines.map((e) => e.toString()).join('\n');
      if (mounted) setState(() {});
    } catch (e) {
      _logs = '$e';
    }
  }

  Future<void> _saveConfig() async {
    final s = widget.state;
    Map<String, dynamic> obj;
    try {
      obj = Map<String, dynamic>.from(jsonDecode(_config.text) as Map);
    } catch (e) {
      s.addLog('✗ 配置 JSON 无效: $e');
      return;
    }
    for (final entry in obj.entries) {
      await s.call('plugins/config/set', <String, dynamic>{
        'id': widget.pluginId,
        'key': entry.key,
        'value': entry.value,
      });
    }
    s.addLog('✓ 插件配置已保存');
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.state;
    final p = widget.plugin;
    final caps = ((p['capabilities'] as List?) ?? <dynamic>[]).map((e) => e.toString()).toList();
    return DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.85,
      builder: (BuildContext context, ScrollController scroll) => ListView(
        controller: scroll,
        padding: const EdgeInsets.symmetric(horizontal: 20),
        children: <Widget>[
          Text('${p['name']}', style: Theme.of(context).textTheme.titleLarge),
          Text(widget.pluginId, style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 6),
          Text('${p['description'] ?? ''}'),
          const SizedBox(height: 8),
          Wrap(
            spacing: 6,
            runSpacing: 6,
            children: caps.map((c) => Chip(label: Text(c), visualDensity: VisualDensity.compact)).toList(),
          ),
          const Divider(height: 24),
          Row(
            children: <Widget>[
              Expanded(
                child: OutlinedButton.icon(
                  icon: const Icon(Icons.delete_outline),
                  label: const Text('卸载'),
                  onPressed: () async {
                    final ok = await showDialog<bool>(
                          context: context,
                          builder: (BuildContext c) => AlertDialog(
                            title: const Text('卸载插件'),
                            content: Text('卸载 ${widget.pluginId}？其目录将被删除。'),
                            actions: <Widget>[
                              TextButton(onPressed: () => Navigator.pop(c, false), child: const Text('取消')),
                              FilledButton(onPressed: () => Navigator.pop(c, true), child: const Text('卸载')),
                            ],
                          ),
                        ) ??
                        false;
                    if (!ok) return;
                    await s.call('plugins/uninstall', <String, dynamic>{'id': widget.pluginId});
                    s.addLog('✓ 已卸载');
                    await s.loadPlugins();
                    if (mounted) Navigator.pop(context);
                  },
                ),
              ),
            ],
          ),
          const Divider(height: 24),
          Text('配置（JSON）', style: Theme.of(context).textTheme.titleSmall),
          TextField(
            controller: _config,
            maxLines: 6,
            style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
            decoration: const InputDecoration(border: OutlineInputBorder()),
          ),
          const SizedBox(height: 8),
          Row(
            children: <Widget>[
              OutlinedButton(onPressed: _loadConfig, child: const Text('读取')),
              const SizedBox(width: 8),
              FilledButton(onPressed: _saveConfig, child: const Text('保存')),
            ],
          ),
          const Divider(height: 24),
          Text('触发 UI 动作', style: Theme.of(context).textTheme.titleSmall),
          Row(
            children: <Widget>[
              Expanded(child: TextField(controller: _action, decoration: const InputDecoration(hintText: 'action', border: OutlineInputBorder()))),
              const SizedBox(width: 8),
              Expanded(child: TextField(controller: _payload, decoration: const InputDecoration(hintText: 'payload（可选）', border: OutlineInputBorder()))),
              const SizedBox(width: 8),
              IconButton.filled(
                icon: const Icon(Icons.send),
                onPressed: () => guard(s, '触发', () async {
                  if (_action.text.trim().isEmpty) return;
                  await s.call('plugins/trigger', <String, dynamic>{
                    'pluginId': widget.pluginId,
                    'action': _action.text.trim(),
                    if (_payload.text.isNotEmpty) 'payload': _payload.text,
                  });
                  s.addLog('✓ 已触发 ui:${_action.text}');
                  await _loadLogs();
                }),
              ),
            ],
          ),
          const Divider(height: 24),
          Row(
            children: <Widget>[
              Text('日志', style: Theme.of(context).textTheme.titleSmall),
              const Spacer(),
              IconButton(icon: const Icon(Icons.refresh), onPressed: _loadLogs),
            ],
          ),
          Container(
            height: 160,
            padding: const EdgeInsets.all(8),
            decoration: BoxDecoration(
              color: const Color(0xFF10131A),
              borderRadius: BorderRadius.circular(8),
            ),
            child: SingleChildScrollView(
              child: SelectableText(_logs.isEmpty ? '（无日志）' : _logs,
                  style: const TextStyle(fontFamily: 'monospace', fontSize: 11)),
            ),
          ),
          const SizedBox(height: 24),
        ],
      ),
    );
  }
}

/* ══════════════════════ settings ══════════════════════ */

class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key, required this.state});
  final AppState state;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  final TextEditingController _port = TextEditingController();
  final TextEditingController _webPort = TextEditingController();
  final TextEditingController _bind = TextEditingController();
  final TextEditingController _device = TextEditingController();
  final TextEditingController _lang = TextEditingController();
  final TextEditingController _color = TextEditingController();
  String _mode = 'wifi';
  bool _autoBind = true;
  bool _muteSync = true;
  bool _loaded = false;

  void _sync() {
    final s = widget.state;
    if (s.prefs.isEmpty) return;
    _port.text = '${s.prefs['port'] ?? 8554}';
    _webPort.text = '${s.prefs['webPort'] ?? 8443}';
    _bind.text = s.prefs['bindAddress']?.toString() ?? '0.0.0.0';
    _device.text = s.prefs['outputDevice']?.toString() ?? '';
    _mode = s.prefs['mode']?.toString() ?? 'wifi';
    _autoBind = s.prefs['autoBind'] != false;
    _muteSync = s.prefs['muteSync'] != false;
    _lang.text = s.uiPrefs['language']?.toString() ?? '';
    _color.text = s.uiPrefs['themeColor']?.toString() ?? '#5b7cfa';
    _loaded = true;
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.state;
    if (!_loaded) _sync();
    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('server.json（连接偏好）', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 12),
                  Wrap(
                    spacing: 12,
                    runSpacing: 12,
                    children: <Widget>[
                      SizedBox(width: 130, child: TextField(controller: _port, keyboardType: TextInputType.number, decoration: const InputDecoration(labelText: '端口', border: OutlineInputBorder()))),
                      SizedBox(width: 130, child: TextField(controller: _webPort, keyboardType: TextInputType.number, decoration: const InputDecoration(labelText: 'Web 端口', border: OutlineInputBorder()))),
                      SizedBox(width: 130, child: TextField(controller: _bind, decoration: const InputDecoration(labelText: '绑定地址', border: OutlineInputBorder(), helperText: '支持 :: / 0.0.0.0 / 具体地址'))),
                      SizedBox(
                        width: 200,
                        child: DropdownButtonFormField<String>(
                          value: _mode,
                          decoration: const InputDecoration(labelText: '模式', border: OutlineInputBorder()),
                          items: const <DropdownMenuItem<String>>[
                            DropdownMenuItem(value: 'wifi', child: Text('wifi')),
                            DropdownMenuItem(value: 'usb', child: Text('usb')),
                            DropdownMenuItem(value: 'web', child: Text('web')),
                          ],
                          onChanged: (v) => setState(() => _mode = v ?? 'wifi'),
                        ),
                      ),
                      SizedBox(width: 260, child: TextField(controller: _device, decoration: const InputDecoration(labelText: '输出设备（留空=默认）', border: OutlineInputBorder()))),
                    ],
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('自动绑定（通配地址）'),
                    value: _autoBind,
                    onChanged: (v) => setState(() => _autoBind = v),
                  ),
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('与手机双向同步静音'),
                    value: _muteSync,
                    onChanged: (v) => setState(() => _muteSync = v),
                  ),
                  const SizedBox(height: 8),
                  Row(
                    children: <Widget>[
                      FilledButton.icon(
                        icon: const Icon(Icons.save),
                        label: const Text('保存'),
                        onPressed: () => guard(s, '保存设置', () async {
                          await s.call('server/prefs/save', <String, dynamic>{
                            'port': int.tryParse(_port.text) ?? 8554,
                            'webPort': int.tryParse(_webPort.text) ?? 8443,
                            'mode': _mode,
                            'bindAddress': _bind.text.isEmpty ? '0.0.0.0' : _bind.text,
                            'autoBind': _autoBind,
                            'outputDevice': _device.text,
                            'muteSync': _muteSync,
                          });
                          s.addLog('✓ server.json 已保存');
                          await s.loadPrefs();
                        }),
                      ),
                      const SizedBox(width: 10),
                      OutlinedButton.icon(
                        icon: const Icon(Icons.refresh),
                        label: const Text('重载'),
                        onPressed: () async {
                          await s.loadPrefs();
                          setState(() {
                            _loaded = false;
                          });
                        },
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('ui.json / theme.json', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 12),
                  Wrap(
                    spacing: 12,
                    children: <Widget>[
                      SizedBox(width: 160, child: TextField(controller: _lang, decoration: const InputDecoration(labelText: '语言', border: OutlineInputBorder()))),
                      SizedBox(width: 160, child: TextField(controller: _color, decoration: const InputDecoration(labelText: '主题色', border: OutlineInputBorder()))),
                    ],
                  ),
                  const SizedBox(height: 10),
                  FilledButton.tonalIcon(
                    icon: const Icon(Icons.save),
                    label: const Text('保存 ui.json'),
                    onPressed: () => guard(s, '保存 UI', () async {
                      await s.call('config/ui/save', <String, dynamic>{
                        'language': _lang.text,
                        'themeColor': _color.text,
                      });
                      s.addLog('✓ ui.json 已保存');
                    }),
                  ),
                  const SizedBox(height: 12),
                  Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    children: s.themeColors.entries
                        .map((e) => Chip(
                              visualDensity: VisualDensity.compact,
                              avatar: Container(
                                width: 12,
                                height: 12,
                                decoration: BoxDecoration(
                                  color: _parseColor(e.value.toString()),
                                  borderRadius: BorderRadius.circular(3),
                                ),
                              ),
                              label: Text('${e.key}: ${e.value}'),
                            ))
                        .toList(),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  static Color _parseColor(String hex) {
    final cleaned = hex.replaceFirst('#', '');
    if (cleaned.length != 6) return Colors.grey;
    final v = int.tryParse(cleaned, radix: 16);
    return v == null ? Colors.grey : Color(0xFF000000 | v);
  }
}

/* ══════════════════════ system ══════════════════════ */

class SystemPage extends StatelessWidget {
  const SystemPage({super.key, required this.state});
  final AppState state;

  @override
  Widget build(BuildContext context) {
    final s = state;
    return PagePad(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('后端 / 模式锁', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 6),
                  Text('libmicyou ${s.version['version'] ?? s.hello['version']} · '
                      '契约 v${s.version['apiVersion'] ?? s.hello['apiVersion']} · '
                      '${s.hello['os']}/${s.hello['arch']}'),
                  Text('模式锁: ${s.modeStatus['mode'] ?? '-'} · pid ${s.modeStatus['pid'] ?? '-'} · '
                      '存活 ${s.modeStatus['running'] == true ? '是' : '否'}'),
                  const SizedBox(height: 10),
                  Wrap(
                    spacing: 10,
                    children: <Widget>[
                      OutlinedButton.icon(
                        icon: const Icon(Icons.refresh),
                        label: const Text('刷新'),
                        onPressed: () => s.loadSystem(),
                      ),
                      OutlinedButton.icon(
                        icon: const Icon(Icons.system_update),
                        label: const Text('检查更新'),
                        onPressed: () => guard(s, '更新检查', () async {
                          final r = Map<String, dynamic>.from(await s.call('system/update/check') as Map);
                          s.updateResult = r;
                          s.notifyListeners();
                        }),
                      ),
                      OutlinedButton.icon(
                        icon: const Icon(Icons.lock_open),
                        label: const Text('释放模式锁'),
                        onPressed: () => guard(s, '模式锁', () async {
                          await s.call('mode/releaseLock');
                          s.addLog('✓ 模式锁已释放');
                          await s.loadSystem();
                        }),
                      ),
                    ],
                  ),
                  if (s.updateResult != null) ...<Widget>[
                    const SizedBox(height: 10),
                    Text('有更新: ${s.updateResult!['hasUpdate'] == true ? '✅' : '否'} · '
                        '${s.updateResult!['currentVersion']} → ${s.updateResult!['latestVersion']}'),
                    SelectableText('${s.updateResult!['releaseUrl']}',
                        style: Theme.of(context).textTheme.bodySmall),
                  ],
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('守护进程日志', style: Theme.of(context).textTheme.titleMedium),
                  const SizedBox(height: 4),
                  SelectableText(s.logPath, style: Theme.of(context).textTheme.bodySmall),
                  const SizedBox(height: 10),
                  Wrap(
                    spacing: 10,
                    children: <Widget>[
                      OutlinedButton.icon(
                        icon: const Icon(Icons.download),
                        label: const Text('加载尾部 64KiB'),
                        onPressed: () => guard(s, '日志', () async {
                          final r = Map<String, dynamic>.from(
                              await s.call('system/log/content', <String, dynamic>{'maxBytes': 65536}) as Map);
                          s.logTail = r['content']?.toString() ?? '';
                          s.notifyListeners();
                        }),
                      ),
                      OutlinedButton.icon(
                        icon: const Icon(Icons.save_alt),
                        label: const Text('导出'),
                        onPressed: () => guard(s, '导出', () async {
                          final r = Map<String, dynamic>.from(await s.call('system/log/export') as Map);
                          s.addLog('✓ 已导出: ${r['path']}');
                        }),
                      ),
                    ],
                  ),
                  const SizedBox(height: 10),
                  Container(
                    height: 260,
                    padding: const EdgeInsets.all(8),
                    decoration: BoxDecoration(
                      color: const Color(0xFF10131A),
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: SingleChildScrollView(
                      child: SelectableText(
                        s.logTail.isEmpty ? '（未加载）' : s.logTail,
                        style: const TextStyle(fontFamily: 'monospace', fontSize: 11),
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}
