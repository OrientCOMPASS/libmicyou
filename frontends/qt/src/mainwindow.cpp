/*
 * libmicyou — Qt 6 reference frontend.
 * Copyright (C) 2026 OrientCOMPASS
 * GPL-3.0-or-later with the MicYou Plugin Exception.
 */
#include "mainwindow.h"
#include "session.h"

#include <QAction>
#include <QApplication>
#include <QCheckBox>
#include <QComboBox>
#include <QDateTime>
#include <QDockWidget>
#include <QFormLayout>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QHeaderView>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMenu>
#include <QMenuBar>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QProgressBar>
#include <QPushButton>
#include <QScrollArea>
#include <QSignalBlocker>
#include <QSlider>
#include <QSpinBox>
#include <QStackedWidget>
#include <QStatusBar>
#include <QSysInfo>
#include <QTableWidget>
#include <QToolBar>
#include <QVBoxLayout>

#include <utility>

namespace {

QString jstr(const QJsonObject &o, const char *key)
{
    return o.value(QLatin1String(key)).toString();
}
double jdbl(const QJsonObject &o, const char *key, double def = 0.0)
{
    return o.value(QLatin1String(key)).toDouble(def);
}
bool jbool(const QJsonObject &o, const char *key)
{
    return o.value(QLatin1String(key)).toBool();
}
int jint(const QJsonObject &o, const char *key, int def = 0)
{
    return o.value(QLatin1String(key)).toInt(def);
}

QLabel *dimLabel(const QString &text)
{
    auto *l = new QLabel(text);
    l->setStyleSheet(QStringLiteral("color: palette(mid);"));
    l->setWordWrap(true);
    return l;
}

QSlider *hSlider(int min, int max, int value)
{
    auto *s = new QSlider(Qt::Horizontal);
    s->setRange(min, max);
    s->setValue(value);
    return s;
}

QScrollArea *scrolled(QWidget *inner)
{
    auto *area = new QScrollArea;
    area->setWidgetResizable(true);
    area->setFrameShape(QFrame::NoFrame);
    area->setWidget(inner);
    return area;
}

} // namespace

MainWindow::MainWindow(QWidget *parent) : QMainWindow(parent)
{
    setWindowTitle(QStringLiteral("MicYou · Qt Frontend (libmicyou sidecar)"));
    resize(1080, 720);

    m_session = new Session(this);
    connect(m_session, &Session::eventReceived, this, &MainWindow::onEvent);
    connect(m_session, &Session::disconnected, this, [this](int) {
        logEvent(QStringLiteral("守护进程已断开"));
        m_phaseLabel->setText(QStringLiteral("offline"));
    });

    // ── menus ──────────────────────────────────────────────────────────
    QMenu *fileMenu = menuBar()->addMenu(QStringLiteral("文件(&F)"));
    fileMenu->addAction(QStringLiteral("退出(&Q)"), this, &QWidget::close);
    QMenu *serverMenu = menuBar()->addMenu(QStringLiteral("服务器(&S)"));
    QAction *actStart = serverMenu->addAction(QStringLiteral("启动(&S)"));
    QAction *actStop = serverMenu->addAction(QStringLiteral("停止(&T)"));
    serverMenu->addSeparator();
    m_actMute = serverMenu->addAction(QStringLiteral("静音(&M)"));
    m_actMute->setCheckable(true);
    m_actMonitor = serverMenu->addAction(QStringLiteral("耳返监听(&O)"));
    m_actMonitor->setCheckable(true);
    QMenu *helpMenu = menuBar()->addMenu(QStringLiteral("帮助(&H)"));
    helpMenu->addAction(QStringLiteral("关于(&A)"), this, [this] {
        QMessageBox::about(this, QStringLiteral("MicYou Qt 前端"),
                           QStringLiteral("libmicyou 后端参考前端（Qt Widgets）。\n"
                                          "JSON-RPC over stdio · GPL-3.0 + 插件例外"));
    });
    connect(actStart, &QAction::triggered, this, &MainWindow::doStart);
    connect(actStop, &QAction::triggered, this, &MainWindow::doStop);
    connect(m_actMute, &QAction::toggled, this, &MainWindow::setMuted);
    connect(m_actMonitor, &QAction::toggled, this, &MainWindow::setMonitoring);

    // ── toolbar ────────────────────────────────────────────────────────
    QToolBar *bar = addToolBar(QStringLiteral("main"));
    bar->setMovable(false);
    bar->addAction(actStart);
    bar->addAction(actStop);
    bar->addSeparator();
    bar->addAction(m_actMute);
    bar->addAction(m_actMonitor);
    bar->addSeparator();
    bar->addWidget(new QLabel(QStringLiteral(" 端口 ")));
    m_qPort = new QLineEdit(QStringLiteral("18554"));
    m_qPort->setFixedWidth(64);
    bar->addWidget(m_qPort);
    bar->addWidget(new QLabel(QStringLiteral(" 模式 ")));
    m_qMode = new QComboBox;
    m_qMode->addItems({QStringLiteral("wifi"), QStringLiteral("usb"), QStringLiteral("web")});
    bar->addWidget(m_qMode);

    // ── status bar telemetry ───────────────────────────────────────────
    m_phaseLabel = new QLabel(QStringLiteral("connecting…"));
    m_deviceLabel = new QLabel(QStringLiteral("无设备"));
    m_levelBar = new QProgressBar;
    m_levelBar->setRange(0, 100);
    m_levelBar->setFixedWidth(160);
    m_levelBar->setTextVisible(false);
    statusBar()->addWidget(m_phaseLabel);
    statusBar()->addPermanentWidget(m_levelBar);
    statusBar()->addPermanentWidget(m_deviceLabel);

    // ── central: list navigation + stacked pages ───────────────────────
    auto *central = new QWidget;
    auto *hbox = new QHBoxLayout(central);
    hbox->setContentsMargins(6, 6, 6, 6);
    m_nav = new QListWidget;
    m_nav->setFixedWidth(150);
    m_nav->addItems({
        QStringLiteral("总览"),
        QStringLiteral("音频与 DSP"),
        QStringLiteral("连接"),
        QStringLiteral("虚拟设备"),
        QStringLiteral("插件"),
        QStringLiteral("设置"),
        QStringLiteral("系统"),
    });
    m_pages = new QStackedWidget;
    m_pages->addWidget(buildOverviewPage());
    m_pages->addWidget(buildAudioPage());
    m_pages->addWidget(buildConnectionPage());
    m_pages->addWidget(buildDevicesPage());
    m_pages->addWidget(buildPluginsPage());
    m_pages->addWidget(buildSettingsPage());
    m_pages->addWidget(buildSystemPage());
    hbox->addWidget(m_nav);
    hbox->addWidget(m_pages, 1);
    setCentralWidget(central);
    connect(m_nav, &QListWidget::currentRowChanged, this, &MainWindow::navigate);

    // ── dockable event log ─────────────────────────────────────────────
    auto *dock = new QDockWidget(QStringLiteral("事件日志"), this);
    dock->setAllowedAreas(Qt::BottomDockWidgetArea | Qt::TopDockWidgetArea);
    m_eventLog = new QPlainTextEdit;
    m_eventLog->setReadOnly(true);
    m_eventLog->setMaximumBlockCount(500);
    dock->setWidget(m_eventLog);
    addDockWidget(Qt::BottomDockWidgetArea, dock);

    // ── boot the session ───────────────────────────────────────────────
    QString err;
    if (!m_session->start(&err)) {
        logEvent(QStringLiteral("✗ 守护进程启动失败: %1").arg(err));
        m_phaseLabel->setText(QStringLiteral("daemon missing"));
        QMessageBox::warning(this, QStringLiteral("后端未连接"), err);
        return;
    }
    logEvent(QStringLiteral("✓ 守护进程已连接（stdio JSON-RPC）"));
    m_session->call(QStringLiteral("session/hello"), QJsonObject{},
                    [this](const QJsonValue &result, const QString &) {
        const QJsonObject info = result.toObject();
        m_os = jstr(info, "os");
        logEvent(QStringLiteral("后端: %1 %2 · api v%3 · %4/%5")
                     .arg(jstr(info, "backend"), jstr(info, "version"))
                     .arg(info.value("apiVersion").toInt())
                     .arg(m_os, jstr(info, "arch")));
        refreshStatus();
        m_nav->setCurrentRow(0);
        if (m_pages->currentIndex() == 0)
            navigate(0);
    });
}

// ── pages ────────────────────────────────────────────────────────────────

QWidget *MainWindow::buildOverviewPage()
{
    auto *page = new QWidget;
    auto *v = new QVBoxLayout(page);
    auto *box = new QGroupBox(QStringLiteral("服务器状态"));
    auto *form = new QFormLayout(box);
    m_ovStatus = dimLabel(QStringLiteral("—"));
    form->addRow(QStringLiteral("状态"), m_ovStatus);
    v->addWidget(box);
    v->addStretch(1);
    return page;
}

QWidget *MainWindow::buildAudioPage()
{
    auto *page = new QWidget;
    auto *v = new QVBoxLayout(page);

    auto *outBox = new QGroupBox(QStringLiteral("输出"));
    auto *outForm = new QFormLayout(outBox);
    m_outDevice = new QComboBox;
    m_bufferMs = new QSpinBox;
    m_bufferMs->setRange(100, 1200);
    m_bufferMs->setSingleStep(50);
    m_bufferMs->setValue(300);
    auto *refreshDevices = new QPushButton(QStringLiteral("刷新设备"));
    auto *outRow = new QHBoxLayout;
    outRow->addWidget(m_outDevice, 1);
    outRow->addWidget(refreshDevices);
    outForm->addRow(QStringLiteral("设备（下次启动生效）"), outRow);
    outForm->addRow(QStringLiteral("缓冲 (ms)"), m_bufferMs);
    v->addWidget(outBox);

    auto *swBox = new QGroupBox(QStringLiteral("开关"));
    auto *swL = new QVBoxLayout(swBox);
    m_chkMute = new QCheckBox(QStringLiteral("硬静音"));
    m_chkMonitor = new QCheckBox(QStringLiteral("耳返监听"));
    m_chkAec = new QCheckBox(QStringLiteral("回声消除 AEC"));
    m_aecStatus = dimLabel(QString());
    swL->addWidget(m_chkMute);
    swL->addWidget(m_chkMonitor);
    swL->addWidget(m_chkAec);
    swL->addWidget(m_aecStatus);
    v->addWidget(swBox);
    connect(m_chkMute, &QCheckBox::toggled, this, &MainWindow::setMuted);
    connect(m_chkMonitor, &QCheckBox::toggled, this, &MainWindow::setMonitoring);

    auto *dspBox = new QGroupBox(QStringLiteral("DSP 处理链"));
    auto *d = new QFormLayout(dspBox);
    m_chainLabel = dimLabel(QStringLiteral("—"));
    d->addRow(QStringLiteral("链"), m_chainLabel);

    auto gainRow = new QHBoxLayout;
    m_gain = hSlider(-500, 500, 0);
    m_gainLabel = new QLabel(QStringLiteral("0.0 dB"));
    gainRow->addWidget(m_gain, 1);
    gainRow->addWidget(m_gainLabel);
    d->addRow(QStringLiteral("增益"), gainRow);
    connect(m_gain, &QSlider::valueChanged, this, [this](int v) {
        m_gainLabel->setText(QStringLiteral("%1 dB").arg(v / 10.0, 0, 'f', 1));
    });

    m_nsEn = new QCheckBox(QStringLiteral("启用降噪"));
    m_nsType = new QComboBox;
    m_nsType->addItems({QStringLiteral("PureVox"), QStringLiteral("RNNoise"), QStringLiteral("Speexdsp")});
    m_nsIntensity = hSlider(0, 100, 50);
    d->addRow(QStringLiteral("降噪 NS"), m_nsEn);
    d->addRow(QStringLiteral("NS 类型"), m_nsType);
    d->addRow(QStringLiteral("NS 强度"), m_nsIntensity);

    m_drvEn = new QCheckBox(QStringLiteral("启用去混响"));
    m_drvLevel = hSlider(0, 100, 50);
    d->addRow(QStringLiteral("去混响"), m_drvEn);
    d->addRow(QStringLiteral("强度"), m_drvLevel);

    m_agcEn = new QCheckBox(QStringLiteral("启用 AGC"));
    m_agcTarget = new QSpinBox;
    m_agcTarget->setRange(0, 32767);
    m_agcTarget->setSingleStep(500);
    m_agcTarget->setValue(16000);
    m_agcAttack = hSlider(1, 100, 50);
    m_agcDecay = hSlider(1, 100, 50);
    d->addRow(QStringLiteral("自动增益"), m_agcEn);
    d->addRow(QStringLiteral("AGC 目标"), m_agcTarget);
    d->addRow(QStringLiteral("Attack"), m_agcAttack);
    d->addRow(QStringLiteral("Decay"), m_agcDecay);

    m_vadEn = new QCheckBox(QStringLiteral("启用 VAD"));
    m_vadThreshold = hSlider(-100, 0, -40);
    d->addRow(QStringLiteral("语音门限"), m_vadEn);
    d->addRow(QStringLiteral("阈值 (dB)"), m_vadThreshold);

    m_eqEn = new QCheckBox(QStringLiteral("启用 EQ"));
    m_eqPreamp = hSlider(-120, 120, 0);
    d->addRow(QStringLiteral("均衡器"), m_eqEn);
    d->addRow(QStringLiteral("前置放大 (×0.1dB)"), m_eqPreamp);

    // 10 vertical band sliders — native Qt feel
    auto *eqRow = new QHBoxLayout;
    const char *labels[] = {"31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"};
    m_eqBands.clear();
    for (const char *lbl : labels) {
        auto *cell = new QVBoxLayout;
        auto *cap = new QLabel(QStringLiteral("%1Hz").arg(QLatin1String(lbl)));
        cap->setAlignment(Qt::AlignCenter);
        auto *slider = new QSlider(Qt::Vertical);
        slider->setRange(-120, 120);
        slider->setValue(0);
        slider->setFixedHeight(96);
        slider->setTickPosition(QSlider::TicksBothSides);
        cell->addWidget(cap);
        cell->addWidget(slider, 1, Qt::AlignHCenter);
        eqRow->addLayout(cell);
        m_eqBands.append(slider);
    }
    d->addRow(QStringLiteral("EQ 频段"), eqRow);

    auto *btnRow = new QHBoxLayout;
    auto *save = new QPushButton(QStringLiteral("保存并应用"));
    auto *reload = new QPushButton(QStringLiteral("重载"));
    btnRow->addWidget(save);
    btnRow->addWidget(reload);
    btnRow->addStretch(1);
    d->addRow(btnRow);
    connect(save, &QPushButton::clicked, this, &MainWindow::saveDsp);
    connect(reload, &QPushButton::clicked, this, &MainWindow::loadAudioPage);
    connect(refreshDevices, &QPushButton::clicked, this, &MainWindow::loadAudioPage);
    connect(m_outDevice, qOverload<int>(&QComboBox::activated), this, [this](int idx) {
        const QString device = idx > 0 && idx - 1 < m_deviceCache.size()
                                   ? m_deviceCache.at(idx - 1)
                                   : QString();
        m_session->call(QStringLiteral("server/prefs/get"), QJsonObject{},
                        [this, device](const QJsonValue &r, const QString &) {
            QJsonObject prefs = r.toObject();
            prefs.insert(QStringLiteral("outputDevice"), device);
            m_session->call(QStringLiteral("server/prefs/save"), prefs,
                            [this](const QJsonValue &, const QString &err) {
                logEvent(err.isEmpty() ? QStringLiteral("✓ 输出设备已保存（下次启动生效）")
                                       : QStringLiteral("✗ %1").arg(err));
            });
        });
    });
    v->addWidget(scrolled(dspBox), 1);
    return scrolled(page);
}

QWidget *MainWindow::buildConnectionPage()
{
    auto *page = new QWidget;
    auto *v = new QVBoxLayout(page);

    auto *netBox = new QGroupBox(QStringLiteral("局域网地址（手机连接用）"));
    auto *nv = new QVBoxLayout(netBox);
    m_netIps = new QLabel(QStringLiteral("—"));
    m_netIps->setTextInteractionFlags(Qt::TextSelectableByMouse);
    nv->addWidget(m_netIps);
    auto *fw = new QPushButton(QStringLiteral("允许防火墙入站（Windows UAC）"));
    nv->addWidget(fw, 0, Qt::AlignLeft);
    connect(fw, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("network/firewall/allow"), QJsonObject{},
                        [this](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ 已请求防火墙规则")
                                   : QStringLiteral("✗ 防火墙: %1").arg(err));
        });
    });
    v->addWidget(netBox);

    auto *ifBox = new QGroupBox(QStringLiteral("网络接口"));
    auto *iv = new QVBoxLayout(ifBox);
    m_ifaceTable = new QTableWidget(0, 2);
    m_ifaceTable->setHorizontalHeaderLabels({QStringLiteral("IP"), QStringLiteral("接口")});
    m_ifaceTable->horizontalHeader()->setStretchLastSection(true);
    m_ifaceTable->verticalHeader()->setVisible(false);
    m_ifaceTable->setEditTriggers(QAbstractItemView::NoEditTriggers);
    iv->addWidget(m_ifaceTable);
    v->addWidget(ifBox, 1);

    auto *usbBox = new QGroupBox(QStringLiteral("USB（adb reverse）"));
    auto *uv = new QHBoxLayout(usbBox);
    m_usbDevices = new QComboBox;
    auto *usbRefresh = new QPushButton(QStringLiteral("刷新"));
    auto *usbEnable = new QPushButton(QStringLiteral("启用 USB 模式"));
    uv->addWidget(m_usbDevices, 1);
    uv->addWidget(usbRefresh);
    uv->addWidget(usbEnable);
    v->addWidget(usbBox);
    m_usbResult = dimLabel(QString());
    v->addWidget(m_usbResult);
    connect(usbRefresh, &QPushButton::clicked, this, &MainWindow::loadConnectionPage);
    connect(usbEnable, &QPushButton::clicked, this, [this] {
        QJsonObject params{{"port", m_qPort->text().toUShort()}};
        const int idx = m_usbDevices->currentIndex();
        if (idx > 0 && idx - 1 < m_usbSerials.size())
            params.insert(QStringLiteral("deviceSerial"), m_usbSerials.at(idx - 1));
        m_session->call(QStringLiteral("usb/enable"), params,
                        [this](const QJsonValue &r, const QString &err) {
            m_usbResult->setText(err.isEmpty()
                                     ? QStringLiteral("✓ %1").arg(QString::fromUtf8(QJsonDocument(r.toObject()).toJson(QJsonDocument::Compact)))
                                     : QStringLiteral("✗ %1").arg(err));
        });
    });

    auto *webBox = new QGroupBox(QStringLiteral("Web 模式"));
    auto *wv = new QVBoxLayout(webBox);
    m_webStatus = dimLabel(QStringLiteral("—"));
    m_webUrls = new QLabel;
    m_webUrls->setTextInteractionFlags(Qt::TextSelectableByMouse);
    m_webUrls->setWordWrap(true);
    auto *webRefresh = new QPushButton(QStringLiteral("刷新"));
    wv->addWidget(m_webStatus);
    wv->addWidget(m_webUrls);
    wv->addWidget(webRefresh, 0, Qt::AlignLeft);
    v->addWidget(webBox);
    connect(webRefresh, &QPushButton::clicked, this, &MainWindow::loadConnectionPage);
    return scrolled(page);
}

QWidget *MainWindow::buildDevicesPage()
{
    auto *page = new QWidget;
    auto *v = new QVBoxLayout(page);

    auto *vbc = new QGroupBox(QStringLiteral("VB-CABLE（Windows）"));
    auto *vbcL = new QVBoxLayout(vbc);
    m_vbcStatus = dimLabel(QStringLiteral("—"));
    auto *vbcCheck = new QPushButton(QStringLiteral("检查"));
    auto *vbcInstall = new QPushButton(QStringLiteral("下载并安装（UAC）"));
    auto *vbcRow = new QHBoxLayout;
    vbcRow->addWidget(vbcCheck);
    vbcRow->addWidget(vbcInstall);
    vbcRow->addStretch(1);
    m_vbcProgress = new QPlainTextEdit;
    m_vbcProgress->setReadOnly(true);
    m_vbcProgress->setFixedHeight(90);
    vbcL->addWidget(m_vbcStatus);
    vbcL->addLayout(vbcRow);
    vbcL->addWidget(m_vbcProgress);
    v->addWidget(vbc);
    connect(vbcCheck, &QPushButton::clicked, this, &MainWindow::loadDevicesPage);
    connect(vbcInstall, &QPushButton::clicked, this, [this, vbcInstall] {
        vbcInstall->setEnabled(false);
        m_vbcProgress->appendPlainText(QStringLiteral("下载安装中…"));
        m_session->call(QStringLiteral("devices/vbcable/install"), QJsonObject{},
                        [this, vbcInstall](const QJsonValue &r, const QString &err) {
            vbcInstall->setEnabled(true);
            const QJsonObject o = r.toObject();
            m_vbcStatus->setText(err.isEmpty()
                                     ? (jbool(o, "success") ? QStringLiteral("✅ 安装成功")
                                                            : QStringLiteral("❌ %1 %2").arg(jstr(o, "errorType"), jstr(o, "message")))
                                     : QStringLiteral("✗ %1").arg(err));
            loadDevicesPage();
        });
    });

    auto *bh = new QGroupBox(QStringLiteral("BlackHole（macOS）"));
    auto *bhL = new QVBoxLayout(bh);
    m_bhStatus = dimLabel(QStringLiteral("—"));
    auto *bhCheck = new QPushButton(QStringLiteral("检查"));
    auto *bhSet = new QPushButton(QStringLiteral("设为系统输入"));
    auto *bhRestore = new QPushButton(QStringLiteral("恢复原输入"));
    auto *bhRow = new QHBoxLayout;
    bhRow->addWidget(bhCheck);
    bhRow->addWidget(bhSet);
    bhRow->addWidget(bhRestore);
    bhRow->addStretch(1);
    bhL->addWidget(m_bhStatus);
    bhL->addLayout(bhRow);
    v->addWidget(bh);
    connect(bhCheck, &QPushButton::clicked, this, &MainWindow::loadDevicesPage);
    connect(bhSet, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("devices/blackhole/setInput"), QJsonObject{},
                        [this](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ BlackHole 已设为输入") : QStringLiteral("✗ %1").arg(err));
            loadDevicesPage();
        });
    });
    connect(bhRestore, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("devices/blackhole/restore"), QJsonObject{},
                        [this](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ 输入设备已恢复") : QStringLiteral("✗ %1").arg(err));
            loadDevicesPage();
        });
    });

    auto *pw = new QGroupBox(QStringLiteral("PipeWire（Linux）"));
    auto *pwL = new QVBoxLayout(pw);
    m_pwStatus = dimLabel(QStringLiteral("—"));
    auto *pwCheck = new QPushButton(QStringLiteral("刷新状态"));
    pwL->addWidget(m_pwStatus);
    pwL->addWidget(pwCheck, 0, Qt::AlignLeft);
    pwL->addWidget(dimLabel(QStringLiteral("服务器启动时自动创建 MicYouVirtualSink/Source。")));
    v->addWidget(pw);
    connect(pwCheck, &QPushButton::clicked, this, &MainWindow::loadDevicesPage);
    v->addStretch(1);

    vbc->setVisible(false);
    bh->setVisible(false);
    pw->setVisible(false);
    vbc->setObjectName(QStringLiteral("dev-win"));
    bh->setObjectName(QStringLiteral("dev-mac"));
    pw->setObjectName(QStringLiteral("dev-linux"));
    return scrolled(page);
}

QWidget *MainWindow::buildPluginsPage()
{
    auto *page = new QWidget;
    auto *h = new QHBoxLayout(page);

    auto *listBox = new QGroupBox(QStringLiteral("已安装插件"));
    auto *lv = new QVBoxLayout(listBox);
    m_pluginList = new QListWidget;
    auto *refresh = new QPushButton(QStringLiteral("刷新"));
    auto *openDir = new QPushButton(QStringLiteral("打开目录"));
    lv->addWidget(m_pluginList, 1);
    lv->addWidget(refresh);
    lv->addWidget(openDir);
    listBox->setFixedWidth(300);
    h->addWidget(listBox);
    connect(refresh, &QPushButton::clicked, this, &MainWindow::loadPlugins);
    connect(openDir, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("plugins/dir"), QJsonObject{},
                        [this](const QJsonValue &r, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("插件目录: %1").arg(jstr(r.toObject(), "path"))
                                   : QStringLiteral("✗ %1").arg(err));
        });
    });
    connect(m_pluginList, &QListWidget::currentRowChanged, this, &MainWindow::selectPlugin);

    auto *detail = new QGroupBox(QStringLiteral("插件详情"));
    auto *dv = new QVBoxLayout(detail);
    m_pdMeta = dimLabel(QStringLiteral("选择左侧插件"));
    m_pdCaps = dimLabel(QString());
    m_pdEnabled = new QCheckBox(QStringLiteral("启用"));
    auto *uninstall = new QPushButton(QStringLiteral("卸载"));
    auto *enRow = new QHBoxLayout;
    enRow->addWidget(m_pdEnabled);
    enRow->addWidget(uninstall);
    enRow->addStretch(1);
    m_pdConfig = new QPlainTextEdit;
    m_pdConfig->setFixedHeight(110);
    auto *cfgLoad = new QPushButton(QStringLiteral("读取配置"));
    auto *cfgSave = new QPushButton(QStringLiteral("保存配置"));
    auto *cfgRow = new QHBoxLayout;
    cfgRow->addWidget(cfgLoad);
    cfgRow->addWidget(cfgSave);
    cfgRow->addStretch(1);
    m_pdAction = new QLineEdit;
    m_pdAction->setPlaceholderText(QStringLiteral("action，如 play"));
    m_pdPayload = new QLineEdit;
    m_pdPayload->setPlaceholderText(QStringLiteral("payload（可选）"));
    auto *trigger = new QPushButton(QStringLiteral("触发"));
    auto *trigRow = new QHBoxLayout;
    trigRow->addWidget(m_pdAction);
    trigRow->addWidget(m_pdPayload, 1);
    trigRow->addWidget(trigger);
    m_pdLog = new QPlainTextEdit;
    m_pdLog->setReadOnly(true);
    auto *logsBtn = new QPushButton(QStringLiteral("刷新日志"));
    dv->addWidget(m_pdMeta);
    dv->addWidget(m_pdCaps);
    dv->addLayout(enRow);
    dv->addWidget(dimLabel(QStringLiteral("配置（JSON，逐键保存）")));
    dv->addWidget(m_pdConfig);
    dv->addLayout(cfgRow);
    dv->addWidget(dimLabel(QStringLiteral("触发 UI 动作")));
    dv->addLayout(trigRow);
    auto *logRow = new QHBoxLayout;
    logRow->addWidget(dimLabel(QStringLiteral("插件日志")));
    logRow->addWidget(logsBtn);
    dv->addLayout(logRow);
    dv->addWidget(m_pdLog, 1);
    h->addWidget(detail, 1);

    connect(m_pdEnabled, &QCheckBox::toggled, this, [this](bool on) {
        if (m_selectedPlugin < 0 || m_selectedPlugin >= m_pluginCache.size())
            return;
        const QString id = m_pluginCache.at(m_selectedPlugin).toObject()
                               .value(QStringLiteral("id")).toString();
        m_session->call(QStringLiteral("plugins/setEnabled"),
                        QJsonObject{{"id", id}, {"enabled", on}},
                        [this](const QJsonValue &, const QString &err) {
            if (!err.isEmpty())
                logEvent(QStringLiteral("✗ 插件开关: %1").arg(err));
            loadPlugins();
        });
    });
    connect(uninstall, &QPushButton::clicked, this, [this] {
        if (m_selectedPlugin < 0 || m_selectedPlugin >= m_pluginCache.size())
            return;
        const QString id = m_pluginCache.at(m_selectedPlugin).toObject()
                               .value(QStringLiteral("id")).toString();
        if (QMessageBox::question(this, QStringLiteral("卸载插件"),
                                  QStringLiteral("卸载 %1？其目录将被删除。").arg(id))
            != QMessageBox::Yes)
            return;
        m_session->call(QStringLiteral("plugins/uninstall"), QJsonObject{{"id", id}},
                        [this](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ 已卸载") : QStringLiteral("✗ %1").arg(err));
            m_selectedPlugin = -1;
            loadPlugins();
        });
    });
    connect(cfgLoad, &QPushButton::clicked, this, [this] {
        if (m_selectedPlugin < 0 || m_selectedPlugin >= m_pluginCache.size())
            return;
        const QString id = m_pluginCache.at(m_selectedPlugin).toObject()
                               .value(QStringLiteral("id")).toString();
        m_session->call(QStringLiteral("plugins/config/get"), QJsonObject{{"id", id}},
                        [this](const QJsonValue &r, const QString &err) {
            m_pdConfig->setPlainText(err.isEmpty()
                                         ? QString::fromUtf8(QJsonDocument(r.toObject()).toJson(QJsonDocument::Indented))
                                         : QStringLiteral("// %1").arg(err));
        });
    });
    connect(cfgSave, &QPushButton::clicked, this, [this] {
        if (m_selectedPlugin < 0 || m_selectedPlugin >= m_pluginCache.size())
            return;
        const QString id = m_pluginCache.at(m_selectedPlugin).toObject()
                               .value(QStringLiteral("id")).toString();
        QJsonParseError perr;
        const QJsonDocument doc = QJsonDocument::fromJson(m_pdConfig->toPlainText().toUtf8(), &perr);
        if (perr.error != QJsonParseError::NoError || !doc.isObject()) {
            logEvent(QStringLiteral("✗ 配置 JSON 无效: %1").arg(perr.errorString()));
            return;
        }
        const QJsonObject obj = doc.object();
        for (auto it = obj.begin(); it != obj.end(); ++it) {
            m_session->call(QStringLiteral("plugins/config/set"),
                            QJsonObject{{"id", id}, {"key", it.key()}, {"value", it.value()}},
                            [this, key = it.key()](const QJsonValue &, const QString &err) {
                if (!err.isEmpty())
                    logEvent(QStringLiteral("✗ 写 %1: %2").arg(key, err));
            });
        }
        logEvent(QStringLiteral("✓ 插件配置已保存"));
    });
    connect(trigger, &QPushButton::clicked, this, [this] {
        if (m_selectedPlugin < 0 || m_selectedPlugin >= m_pluginCache.size())
            return;
        const QString id = m_pluginCache.at(m_selectedPlugin).toObject()
                               .value(QStringLiteral("id")).toString();
        const QString action = m_pdAction->text().trimmed();
        if (action.isEmpty())
            return;
        QJsonObject params{{"pluginId", id}, {"action", action}};
        if (!m_pdPayload->text().isEmpty())
            params.insert(QStringLiteral("payload"), m_pdPayload->text());
        m_session->call(QStringLiteral("plugins/trigger"), params,
                        [this, action](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ 已触发 ui:%1").arg(action)
                                   : QStringLiteral("✗ %1").arg(err));
        });
    });
    connect(logsBtn, &QPushButton::clicked, this, [this] {
        if (m_selectedPlugin < 0 || m_selectedPlugin >= m_pluginCache.size())
            return;
        const QString id = m_pluginCache.at(m_selectedPlugin).toObject()
                               .value(QStringLiteral("id")).toString();
        m_session->call(QStringLiteral("plugins/logs"), QJsonObject{{"id", id}},
                        [this](const QJsonValue &r, const QString &) {
            QStringList lines;
            for (const QJsonValue &l : r.toArray())
                lines << l.toString();
            m_pdLog->setPlainText(lines.join(QLatin1Char('\n')));
        });
    });
    return page;
}

QWidget *MainWindow::buildSettingsPage()
{
    auto *page = new QWidget;
    auto *v = new QVBoxLayout(page);

    auto *srvBox = new QGroupBox(QStringLiteral("server.json（连接偏好）"));
    auto *form = new QFormLayout(srvBox);
    m_spPort = new QSpinBox;
    m_spPort->setRange(1, 65534);
    m_spPort->setValue(8554);
    m_spWebPort = new QSpinBox;
    m_spWebPort->setRange(1, 65535);
    m_spWebPort->setValue(8443);
    m_spMode = new QComboBox;
    m_spMode->addItems({QStringLiteral("wifi"), QStringLiteral("usb"), QStringLiteral("web")});
    m_spBind = new QLineEdit(QStringLiteral("0.0.0.0"));
    m_spAutoBind = new QCheckBox(QStringLiteral("自动绑定（0.0.0.0/::）"));
    m_spDevice = new QLineEdit;
    m_spDevice->setPlaceholderText(QStringLiteral("留空 = 默认/虚拟设备"));
    m_spMuteSync = new QCheckBox(QStringLiteral("与手机双向同步静音"));
    form->addRow(QStringLiteral("端口"), m_spPort);
    form->addRow(QStringLiteral("Web 端口"), m_spWebPort);
    form->addRow(QStringLiteral("模式"), m_spMode);
    form->addRow(QStringLiteral("绑定地址"), m_spBind);
    form->addRow(QString(), m_spAutoBind);
    form->addRow(QStringLiteral("输出设备"), m_spDevice);
    form->addRow(QString(), m_spMuteSync);
    m_prefsExists = dimLabel(QString());
    form->addRow(m_prefsExists);
    auto *saveRow = new QHBoxLayout;
    auto *prefsSave = new QPushButton(QStringLiteral("保存"));
    auto *prefsReload = new QPushButton(QStringLiteral("重载"));
    saveRow->addWidget(prefsSave);
    saveRow->addWidget(prefsReload);
    saveRow->addStretch(1);
    form->addRow(saveRow);
    v->addWidget(srvBox);
    connect(prefsSave, &QPushButton::clicked, this, &MainWindow::savePrefs);
    connect(prefsReload, &QPushButton::clicked, this, &MainWindow::loadSettingsPage);

    auto *uiBox = new QGroupBox(QStringLiteral("ui.json / theme.json"));
    auto *uform = new QFormLayout(uiBox);
    m_uiLang = new QLineEdit;
    m_uiColor = new QLineEdit(QStringLiteral("#5b7cfa"));
    uform->addRow(QStringLiteral("语言"), m_uiLang);
    uform->addRow(QStringLiteral("主题色"), m_uiColor);
    auto *uiSave = new QPushButton(QStringLiteral("保存 ui.json"));
    uform->addRow(uiSave);
    m_themeSummary = dimLabel(QString());
    uform->addRow(QStringLiteral("色板"), m_themeSummary);
    v->addWidget(uiBox);
    connect(uiSave, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("config/ui/save"),
                        QJsonObject{{"language", m_uiLang->text()}, {"themeColor", m_uiColor->text()}},
                        [this](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ ui.json 已保存") : QStringLiteral("✗ %1").arg(err));
        });
    });
    v->addStretch(1);
    return scrolled(page);
}

QWidget *MainWindow::buildSystemPage()
{
    auto *page = new QWidget;
    auto *v = new QVBoxLayout(page);

    auto *verBox = new QGroupBox(QStringLiteral("后端 / 模式锁"));
    auto *vv = new QVBoxLayout(verBox);
    m_sysVersion = dimLabel(QStringLiteral("—"));
    m_sysMode = dimLabel(QStringLiteral("—"));
    m_sysUpdate = dimLabel(QString());
    auto *row = new QHBoxLayout;
    auto *refresh = new QPushButton(QStringLiteral("刷新"));
    auto *update = new QPushButton(QStringLiteral("检查更新"));
    auto *release = new QPushButton(QStringLiteral("释放模式锁"));
    row->addWidget(refresh);
    row->addWidget(update);
    row->addWidget(release);
    row->addStretch(1);
    vv->addWidget(m_sysVersion);
    vv->addWidget(m_sysMode);
    vv->addLayout(row);
    vv->addWidget(m_sysUpdate);
    v->addWidget(verBox);
    connect(refresh, &QPushButton::clicked, this, &MainWindow::loadSystemPage);
    connect(update, &QPushButton::clicked, this, [this] {
        m_sysUpdate->setText(QStringLiteral("检查中…"));
        m_session->call(QStringLiteral("system/update/check"), QJsonObject{},
                        [this](const QJsonValue &r, const QString &err) {
            if (!err.isEmpty()) {
                m_sysUpdate->setText(QStringLiteral("✗ %1").arg(err));
                return;
            }
            const QJsonObject o = r.toObject();
            m_sysUpdate->setText(QStringLiteral("有更新: %1   当前 %2 → 最新 %3\n%4")
                                     .arg(jbool(o, "hasUpdate") ? QStringLiteral("✅") : QStringLiteral("否"),
                                          jstr(o, "currentVersion"), jstr(o, "latestVersion"),
                                          jstr(o, "releaseUrl")));
        });
    });
    connect(release, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("mode/releaseLock"), QJsonObject{},
                        [this](const QJsonValue &, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ 模式锁已释放") : QStringLiteral("✗ %1").arg(err));
            loadSystemPage();
        });
    });

    auto *logBox = new QGroupBox(QStringLiteral("守护进程日志"));
    auto *lv = new QVBoxLayout(logBox);
    m_logPath = dimLabel(QStringLiteral("—"));
    auto *lrow = new QHBoxLayout;
    auto *logRefresh = new QPushButton(QStringLiteral("加载尾部"));
    auto *logExport = new QPushButton(QStringLiteral("导出"));
    lrow->addWidget(logRefresh);
    lrow->addWidget(logExport);
    lrow->addStretch(1);
    m_logContent = new QPlainTextEdit;
    m_logContent->setReadOnly(true);
    lv->addWidget(m_logPath);
    lv->addLayout(lrow);
    lv->addWidget(m_logContent, 1);
    v->addWidget(logBox, 1);
    connect(logRefresh, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("system/log/content"),
                        QJsonObject{{"maxBytes", 65536}},
                        [this](const QJsonValue &r, const QString &err) {
            m_logContent->setPlainText(err.isEmpty() ? jstr(r.toObject(), "content") : err);
        });
    });
    connect(logExport, &QPushButton::clicked, this, [this] {
        m_session->call(QStringLiteral("system/log/export"), QJsonObject{},
                        [this](const QJsonValue &r, const QString &err) {
            logEvent(err.isEmpty() ? QStringLiteral("✓ 已导出: %1").arg(jstr(r.toObject(), "path"))
                                   : QStringLiteral("✗ %1").arg(err));
        });
    });
    return page;
}

// ── navigation / loaders ─────────────────────────────────────────────────

void MainWindow::navigate(int row)
{
    m_pages->setCurrentIndex(row);
    switch (row) {
    case 0: refreshStatus(); break;
    case 1: loadAudioPage(); break;
    case 2: loadConnectionPage(); break;
    case 3: loadDevicesPage(); break;
    case 4: loadPlugins(); break;
    case 5: loadSettingsPage(); break;
    case 6: loadSystemPage(); break;
    default: break;
    }
}

void MainWindow::refreshStatus()
{
    m_session->call(QStringLiteral("server/status"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject o = r.toObject();
        setStatusPills(jstr(o, "phase"), jbool(o, "isServerRunning"),
                       jbool(o, "isConnected"), jbool(o, "isMuted"));
        {
            QSignalBlocker b1(m_actMute), b2(m_chkMute), b3(m_actMonitor), b4(m_chkMonitor);
            m_actMute->setChecked(jbool(o, "isMuted"));
            if (m_chkMute) m_chkMute->setChecked(jbool(o, "isMuted"));
            m_actMonitor->setChecked(jbool(o, "isMonitoring"));
            if (m_chkMonitor) m_chkMonitor->setChecked(jbool(o, "isMonitoring"));
        }
        m_ovStatus->setText(QStringLiteral("相态 %1 · 运行 %2 · 设备 %3 · 模式 %4:%5")
                                .arg(jstr(o, "phase"),
                                     jbool(o, "isServerRunning") ? QStringLiteral("是") : QStringLiteral("否"),
                                     jbool(o, "isConnected") ? QStringLiteral("已连接") : QStringLiteral("无"),
                                     jstr(o, "mode").isEmpty() ? QStringLiteral("-") : jstr(o, "mode"),
                                     o.contains("port") ? QString::number(jint(o, "port")) : QStringLiteral("-")));
    });
}

void MainWindow::setStatusPills(const QString &phase, bool running, bool connected, bool)
{
    m_phaseLabel->setText(phase);
    m_phaseLabel->setStyleSheet(running ? QStringLiteral("color: #2e7d32; font-weight: 600;")
                                        : QString());
    m_deviceLabel->setText(connected ? QStringLiteral("设备已连接") : QStringLiteral("无设备"));
}

void MainWindow::doStart()
{
    const QJsonObject params{
        {"port", static_cast<int>(m_qPort->text().toUShort())},
        {"mode", m_qMode->currentText()},
    };
    logEvent(QStringLiteral("启动服务器 %1 (%2)…").arg(m_qPort->text(), m_qMode->currentText()));
    m_session->call(QStringLiteral("server/start"), params,
                    [this](const QJsonValue &r, const QString &err) {
        logEvent(err.isEmpty() ? QStringLiteral("✓ %1").arg(jstr(r.toObject(), "message"))
                               : QStringLiteral("✗ 启动失败: %1").arg(err));
        refreshStatus();
    });
}

void MainWindow::doStop()
{
    m_session->call(QStringLiteral("server/stop"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &err) {
        logEvent(err.isEmpty() ? QStringLiteral("✓ %1").arg(jstr(r.toObject(), "message"))
                               : QStringLiteral("✗ 停止失败: %1").arg(err));
        refreshStatus();
    });
}

void MainWindow::setMuted(bool muted)
{
    m_session->call(QStringLiteral("audio/mute/set"), QJsonObject{{"muted", muted}},
                    [this, muted](const QJsonValue &, const QString &err) {
        if (!err.isEmpty())
            logEvent(QStringLiteral("✗ 静音: %1").arg(err));
        QSignalBlocker b1(m_actMute), b2(m_chkMute);
        m_actMute->setChecked(muted);
        if (m_chkMute) m_chkMute->setChecked(muted);
    });
}

void MainWindow::setMonitoring(bool enabled)
{
    m_session->call(QStringLiteral("audio/monitoring/set"), QJsonObject{{"enabled", enabled}},
                    [this, enabled](const QJsonValue &, const QString &err) {
        if (!err.isEmpty())
            logEvent(QStringLiteral("✗ 监听: %1").arg(err));
        QSignalBlocker b1(m_actMonitor), b2(m_chkMonitor);
        m_actMonitor->setChecked(enabled);
        if (m_chkMonitor) m_chkMonitor->setChecked(enabled);
    });
}

void MainWindow::loadAudioPage()
{
    m_session->call(QStringLiteral("audio/devices"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        QSignalBlocker b(m_outDevice);
        m_outDevice->clear();
        m_outDevice->addItem(QStringLiteral("（默认 / 虚拟设备）"));
        m_deviceCache.clear();
        for (const QJsonValue &d : r.toArray()) {
            m_outDevice->addItem(d.toString());
            m_deviceCache << d.toString();
        }
        // select current from prefs
        m_session->call(QStringLiteral("server/prefs/get"), QJsonObject{},
                        [this](const QJsonValue &pr, const QString &) {
            const QString dev = jstr(pr.toObject(), "outputDevice");
            const int idx = m_deviceCache.indexOf(dev);
            QSignalBlocker b2(m_outDevice);
            m_outDevice->setCurrentIndex(idx >= 0 ? idx + 1 : 0);
        });
    });
    m_session->call(QStringLiteral("audio/settings/get"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject s = r.toObject();
        m_dspCache = s;
        m_bufferMs->setValue(jint(s, "outputBufferMs", 300));
        m_gain->setValue(static_cast<int>(jdbl(s, "gain") * 10));
        m_nsEn->setChecked(jbool(s, "nsEnabled"));
        m_nsType->setCurrentText(jstr(s, "nsType"));
        m_nsIntensity->setValue(static_cast<int>(jdbl(s, "nsIntensity", 50)));
        m_drvEn->setChecked(jbool(s, "dereverbEnabled"));
        m_drvLevel->setValue(static_cast<int>(jdbl(s, "dereverbLevel", 50)));
        m_agcEn->setChecked(jbool(s, "agcEnabled"));
        m_agcTarget->setValue(static_cast<int>(jdbl(s, "agcTarget", 16000)));
        m_agcAttack->setValue(static_cast<int>(jdbl(s, "agcAttack", 50)));
        m_agcDecay->setValue(static_cast<int>(jdbl(s, "agcDecay", 50)));
        m_vadEn->setChecked(jbool(s, "vadEnabled"));
        m_vadThreshold->setValue(static_cast<int>(jdbl(s, "vadThreshold", -40)));
        m_chkAec->setChecked(jbool(s, "aecEnabled"));
        m_chkAec->setEnabled(m_os != QLatin1String("macos"));
        const QJsonObject eq = s.value(QStringLiteral("equalizer")).toObject();
        m_eqEn->setChecked(jbool(eq, "enabled"));
        m_eqPreamp->setValue(static_cast<int>(jdbl(eq, "preAmp") * 10));
        const QJsonArray gains = eq.value(QStringLiteral("gains")).toArray();
        for (int i = 0; i < m_eqBands.size(); ++i)
            m_eqBands.at(i)->setValue(
                static_cast<int>(gains.size() > i ? gains.at(i).toDouble() * 10 : 0));
        QStringList chain;
        for (const QJsonValue &n : s.value(QStringLiteral("processingChain")).toArray())
            chain << n.toString();
        m_chainLabel->setText(chain.join(QStringLiteral("  →  ")));
    });
}

void MainWindow::saveDsp()
{
    QJsonObject s = m_dspCache; // keep processingChain & unknown fields
    s.insert(QStringLiteral("outputBufferMs"), m_bufferMs->value());
    s.insert(QStringLiteral("gain"), m_gain->value() / 10.0);
    s.insert(QStringLiteral("nsEnabled"), m_nsEn->isChecked());
    s.insert(QStringLiteral("nsType"), m_nsType->currentText());
    s.insert(QStringLiteral("nsIntensity"), m_nsIntensity->value());
    s.insert(QStringLiteral("dereverbEnabled"), m_drvEn->isChecked());
    s.insert(QStringLiteral("dereverbLevel"), m_drvLevel->value());
    s.insert(QStringLiteral("agcEnabled"), m_agcEn->isChecked());
    s.insert(QStringLiteral("agcTarget"), m_agcTarget->value());
    s.insert(QStringLiteral("agcAttack"), m_agcAttack->value());
    s.insert(QStringLiteral("agcDecay"), m_agcDecay->value());
    s.insert(QStringLiteral("vadEnabled"), m_vadEn->isChecked());
    s.insert(QStringLiteral("vadThreshold"), m_vadThreshold->value());
    s.insert(QStringLiteral("aecEnabled"), m_chkAec->isChecked());
    QJsonArray gains;
    for (QSlider *band : std::as_const(m_eqBands))
        gains.append(band->value() / 10.0);
    QJsonObject eq = s.value(QStringLiteral("equalizer")).toObject();
    eq.insert(QStringLiteral("enabled"), m_eqEn->isChecked());
    eq.insert(QStringLiteral("preAmp"), m_eqPreamp->value() / 10.0);
    eq.insert(QStringLiteral("gains"), gains);
    s.insert(QStringLiteral("equalizer"), eq);

    m_session->call(QStringLiteral("audio/settings/update"),
                    QJsonObject{{"settings", s}},
                    [this](const QJsonValue &, const QString &err) {
        logEvent(err.isEmpty() ? QStringLiteral("✓ DSP 设置已保存并热应用")
                               : QStringLiteral("✗ 保存失败: %1").arg(err));
        if (err.isEmpty())
            loadAudioPage();
    });
}

void MainWindow::loadConnectionPage()
{
    m_session->call(QStringLiteral("network/info"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        QStringList ips;
        for (const QJsonValue &ip : r.toObject().value(QStringLiteral("ips")).toArray()) {
            QString s = ip.toString();
            if (s.contains(QLatin1Char(':')))
                s = QLatin1Char('[') + s + QLatin1Char(']');
            ips << s;
        }
        m_netIps->setText(ips.isEmpty() ? QStringLiteral("（无可用接口）") : ips.join(QStringLiteral("   ")));
    });
    m_session->call(QStringLiteral("network/interfaces"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonArray arr = r.toArray();
        m_ifaceTable->setRowCount(arr.size());
        for (int i = 0; i < arr.size(); ++i) {
            const QJsonObject o = arr.at(i).toObject();
            m_ifaceTable->setItem(i, 0, new QTableWidgetItem(jstr(o, "ip")));
            m_ifaceTable->setItem(i, 1, new QTableWidgetItem(jstr(o, "interfaceName")));
        }
    });
    m_session->call(QStringLiteral("usb/devices"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &err) {
        QSignalBlocker b(m_usbDevices);
        m_usbDevices->clear();
        m_usbSerials.clear();
        m_usbDevices->addItem(QStringLiteral("（自动选择）"));
        if (!err.isEmpty()) {
            m_usbResult->setText(QStringLiteral("adb 不可用: %1").arg(err));
            return;
        }
        for (const QJsonValue &d : r.toArray()) {
            const QJsonObject o = d.toObject();
            m_usbDevices->addItem(QStringLiteral("%1 [%2]").arg(jstr(o, "name"), jstr(o, "serial")));
            m_usbSerials << jstr(o, "serial");
        }
    });
    m_session->call(QStringLiteral("web/status"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject o = r.toObject();
        m_webStatus->setText(QStringLiteral("运行: %1   浏览器客户端: %2")
                                 .arg(jbool(o, "running") ? QStringLiteral("是") : QStringLiteral("否"))
                                 .arg(jint(o, "clientCount")));
    });
    m_session->call(QStringLiteral("network/info"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        m_session->call(QStringLiteral("server/prefs/get"), QJsonObject{},
                        [this, r](const QJsonValue &pr, const QString &) {
            const int webPort = jint(pr.toObject(), "webPort", 8443);
            QStringList urls;
            for (const QJsonValue &ip : r.toObject().value(QStringLiteral("ips")).toArray()) {
                QString host = ip.toString();
                if (host.contains(QLatin1Char(':')))
                    host = QLatin1Char('[') + host + QLatin1Char(']');
                urls << QStringLiteral("https://%1:%2").arg(host).arg(webPort);
            }
            m_webUrls->setText(urls.join(QStringLiteral("\n")));
        });
    });
}

void MainWindow::loadDevicesPage()
{
    if (QWidget *w = findChild<QWidget *>(QStringLiteral("dev-win")))
        w->setVisible(m_os == QLatin1String("windows"));
    if (QWidget *w = findChild<QWidget *>(QStringLiteral("dev-mac")))
        w->setVisible(m_os == QLatin1String("macos"));
    if (QWidget *w = findChild<QWidget *>(QStringLiteral("dev-linux")))
        w->setVisible(m_os == QLatin1String("linux"));

    if (m_os == QLatin1String("windows")) {
        m_session->call(QStringLiteral("devices/vbcable/check"), QJsonObject{},
                        [this](const QJsonValue &r, const QString &err) {
            m_vbcStatus->setText(err.isEmpty()
                                     ? (jbool(r.toObject(), "installed") ? QStringLiteral("✅ 已安装")
                                                                         : QStringLiteral("❌ 未安装"))
                                     : QStringLiteral("✗ %1").arg(err));
        });
    } else if (m_os == QLatin1String("macos")) {
        m_session->call(QStringLiteral("devices/blackhole/check"), QJsonObject{},
                        [this](const QJsonValue &r, const QString &err) {
            m_bhStatus->setText(err.isEmpty()
                                    ? QString::fromUtf8(QJsonDocument(r.toObject()).toJson(QJsonDocument::Compact))
                                    : err);
        });
    } else if (m_os == QLatin1String("linux")) {
        m_session->call(QStringLiteral("devices/pipewire/check"), QJsonObject{},
                        [this](const QJsonValue &r, const QString &) {
            const QJsonObject o = r.toObject();
            QString text = QStringLiteral("可用: %1   已建立: %2   设备存在: %3   发行版: %4")
                               .arg(jbool(o, "available") ? QStringLiteral("是") : QStringLiteral("否"),
                                    jbool(o, "setup") ? QStringLiteral("是") : QStringLiteral("否"),
                                    jbool(o, "deviceExists") ? QStringLiteral("是") : QStringLiteral("否"),
                                    jstr(o, "distro"));
            if (!jbool(o, "available") && !jstr(o, "installCommand").isEmpty())
                text += QStringLiteral("\n安装: %1").arg(jstr(o, "installCommand"));
            m_pwStatus->setText(text);
        });
    }
}

void MainWindow::loadPlugins()
{
    m_session->call(QStringLiteral("plugins/list"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &err) {
        if (!err.isEmpty()) {
            logEvent(QStringLiteral("✗ 插件列表: %1").arg(err));
            return;
        }
        m_pluginCache = r.toArray();
        QSignalBlocker b(m_pluginList);
        m_pluginList->clear();
        for (const QJsonValue &pv : std::as_const(m_pluginCache)) {
            const QJsonObject p = pv.toObject();
            auto *item = new QListWidgetItem(
                QStringLiteral("%1  (%2)\n%3 · v%4%5")
                    .arg(jstr(p, "name"), jstr(p, "runtime"), jstr(p, "id"), jstr(p, "version"),
                         jbool(p, "enabled") ? QStringLiteral(" · 已启用") : QString()));
            if (!jstr(p, "error").isEmpty())
                item->setToolTip(jstr(p, "error"));
            m_pluginList->addItem(item);
        }
        if (m_selectedPlugin >= 0 && m_selectedPlugin < m_pluginList->count())
            m_pluginList->setCurrentRow(m_selectedPlugin);
    });
}

void MainWindow::selectPlugin(int row)
{
    m_selectedPlugin = row;
    if (row < 0 || row >= m_pluginCache.size()) {
        m_pdMeta->setText(QStringLiteral("选择左侧插件"));
        return;
    }
    const QJsonObject p = m_pluginCache.at(row).toObject();
    m_pdMeta->setText(QStringLiteral("id: %1\n版本: %2 · 运行时: %3 · 类型: %4 · 已加载: %5\n%6")
                          .arg(jstr(p, "id"), jstr(p, "version"), jstr(p, "runtime"), jstr(p, "kind"),
                               jbool(p, "loaded") ? QStringLiteral("是") : QStringLiteral("否"),
                               jstr(p, "description")));
    QStringList caps;
    for (const QJsonValue &c : p.value(QStringLiteral("capabilities")).toArray())
        caps << c.toString();
    m_pdCaps->setText(caps.join(QStringLiteral("  ")));
    {
        QSignalBlocker b(m_pdEnabled);
        m_pdEnabled->setChecked(jbool(p, "enabled"));
    }
    m_pdConfig->clear();
    m_pdLog->clear();
}

void MainWindow::loadSettingsPage()
{
    m_session->call(QStringLiteral("server/prefs/get"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject p = r.toObject();
        m_spPort->setValue(jint(p, "port", 8554));
        m_spWebPort->setValue(jint(p, "webPort", 8443));
        m_spMode->setCurrentText(jstr(p, "mode"));
        m_spBind->setText(jstr(p, "bindAddress"));
        m_spAutoBind->setChecked(jbool(p, "autoBind"));
        m_spDevice->setText(jstr(p, "outputDevice"));
        m_spMuteSync->setChecked(jbool(p, "muteSync"));
    });
    m_session->call(QStringLiteral("server/prefs/exists"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        m_prefsExists->setText(r.toBool() ? QStringLiteral("server.json 已存在")
                                          : QStringLiteral("server.json 尚未创建（保存后生成）"));
    });
    m_session->call(QStringLiteral("config/ui/get"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject o = r.toObject();
        m_uiLang->setText(jstr(o, "language"));
        const QString color = jstr(o, "themeColor");
        if (!color.isEmpty())
            m_uiColor->setText(color);
    });
    m_session->call(QStringLiteral("config/theme/get"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        QStringList parts;
        const QJsonObject o = r.toObject();
        for (auto it = o.begin(); it != o.end(); ++it)
            parts << QStringLiteral("%1 %2").arg(it.key(), it.value().toString());
        m_themeSummary->setText(parts.join(QStringLiteral("   ")));
    });
}

void MainWindow::savePrefs()
{
    const QJsonObject prefs{
        {"port", m_spPort->value()},
        {"webPort", m_spWebPort->value()},
        {"mode", m_spMode->currentText()},
        {"bindAddress", m_spBind->text().isEmpty() ? QStringLiteral("0.0.0.0") : m_spBind->text()},
        {"autoBind", m_spAutoBind->isChecked()},
        {"outputDevice", m_spDevice->text()},
        {"muteSync", m_spMuteSync->isChecked()},
    };
    m_session->call(QStringLiteral("server/prefs/save"), prefs,
                    [this](const QJsonValue &, const QString &err) {
        logEvent(err.isEmpty() ? QStringLiteral("✓ server.json 已保存") : QStringLiteral("✗ %1").arg(err));
        loadSettingsPage();
    });
}

void MainWindow::loadSystemPage()
{
    m_session->call(QStringLiteral("system/version"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject o = r.toObject();
        m_sysVersion->setText(QStringLiteral("libmicyou %1 · 契约 v%2 · %3")
                                  .arg(jstr(o, "version"))
                                  .arg(o.value(QStringLiteral("apiVersion")).toInt())
                                  .arg(QSysInfo::prettyProductName()));
    });
    m_session->call(QStringLiteral("mode/status"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        const QJsonObject o = r.toObject();
        m_sysMode->setText(QStringLiteral("模式锁: %1 · pid %2 · 存活 %3")
                               .arg(jstr(o, "mode"),
                                    o.contains("pid") ? QString::number(jint(o, "pid")) : QStringLiteral("-"),
                                    jbool(o, "running") ? QStringLiteral("是") : QStringLiteral("否")));
    });
    m_session->call(QStringLiteral("system/log/path"), QJsonObject{},
                    [this](const QJsonValue &r, const QString &) {
        m_logPath->setText(QStringLiteral("路径: %1").arg(jstr(r.toObject(), "path")));
    });
}

// ── events / log ─────────────────────────────────────────────────────────

void MainWindow::logEvent(const QString &line)
{
    m_eventLog->appendPlainText(QStringLiteral("[%1] %2")
                                    .arg(QDateTime::currentDateTime().toString(QStringLiteral("HH:mm:ss")), line));
}

void MainWindow::onEvent(const QString &type, const QJsonObject &data)
{
    if (type == QLatin1String("audioLevel")) {
        const int level = jint(data, "level");
        m_levelBar->setValue(level);
    } else if (type == QLatin1String("muteStateChanged")) {
        const bool muted = jbool(data, "muted");
        QSignalBlocker b1(m_actMute), b2(m_chkMute);
        m_actMute->setChecked(muted);
        if (m_chkMute) m_chkMute->setChecked(muted);
        logEvent(QStringLiteral("静音 → %1").arg(muted ? QStringLiteral("是") : QStringLiteral("否")));
    } else if (type == QLatin1String("monitoringChanged")) {
        const bool on = jbool(data, "enabled");
        QSignalBlocker b1(m_actMonitor), b2(m_chkMonitor);
        m_actMonitor->setChecked(on);
        if (m_chkMonitor) m_chkMonitor->setChecked(on);
        logEvent(QStringLiteral("监听 → %1").arg(on ? QStringLiteral("开") : QStringLiteral("关")));
    } else if (type == QLatin1String("deviceConnected")) {
        const QJsonObject dev = data.value(QStringLiteral("device")).toObject();
        m_deviceLabel->setText(QStringLiteral("设备已连接"));
        logEvent(QStringLiteral("📱 设备连接: %1 (%2)").arg(jstr(dev, "name"), jstr(dev, "ip")));
    } else if (type == QLatin1String("deviceDisconnected")) {
        m_deviceLabel->setText(QStringLiteral("无设备"));
        m_levelBar->setValue(0);
        logEvent(QStringLiteral("设备断开"));
    } else if (type == QLatin1String("serverStopped")) {
        m_levelBar->setValue(0);
        logEvent(QStringLiteral("服务器已停止"));
        refreshStatus();
    } else if (type == QLatin1String("audioMetrics")) {
        const QJsonObject m = data.value(QStringLiteral("metrics")).toObject();
        logEvent(QStringLiteral("指标 延迟%1ms 网络%2ms 抖动%3ms 丢包%4% 码率%5")
                     .arg(jint(m, "latencyMs"))
                     .arg(jint(m, "networkLatencyMs"))
                     .arg(jdbl(m, "jitterMs"), 0, 'f', 1)
                     .arg(jdbl(m, "packetLossRate"), 0, 'f', 2)
                     .arg(jint(m, "bitrate")));
    } else if (type == QLatin1String("udpAudioWarning")) {
        logEvent(QStringLiteral("⚠ 长时间未收到 UDP 音频（防火墙？）"));
    } else if (type == QLatin1String("aecStatusChanged")) {
        const QJsonObject st = data.value(QStringLiteral("status")).toObject();
        m_aecStatus->setText(st.value(QStringLiteral("available")).toBool()
                                 ? (st.value(QStringLiteral("enabled")).toBool()
                                        ? QStringLiteral("AEC: 已启用")
                                        : QStringLiteral("AEC: 可用（未启用）"))
                                 : QStringLiteral("AEC: 不可用 (%1)")
                                       .arg(st.value(QStringLiteral("reason")).toString(QStringLiteral("?"))));
    } else if (type == QLatin1String("installProgress")) {
        m_vbcProgress->appendPlainText(jstr(data, "message"));
    } else if (type == QLatin1String("pluginListChanged")) {
        logEvent(QStringLiteral("插件变更: %1").arg(jstr(data, "pluginId")));
        loadPlugins();
    } else if (type == QLatin1String("pluginLog")) {
        logEvent(QStringLiteral("插件[%1] %2: %3")
                     .arg(jstr(data, "pluginId"), jstr(data, "level"), jstr(data, "message")));
    } else if (type == QLatin1String("pluginDownloadProgress")) {
        logEvent(QStringLiteral("下载 %1: %2/%3%4")
                     .arg(jstr(data, "id"))
                     .arg(static_cast<qint64>(data.value(QStringLiteral("downloaded")).toDouble()))
                     .arg(static_cast<qint64>(data.value(QStringLiteral("total")).toDouble()),
                          jbool(data, "done") ? QStringLiteral(" ✔") : QString()));
    } else if (type == QLatin1String("uiRequest")) {
        logEvent(QStringLiteral("UI 请求（本前端未实现插件面板窗口）: %1")
                     .arg(QString::fromUtf8(QJsonDocument(data.value(QStringLiteral("request")).toObject()).toJson(QJsonDocument::Compact))));
    } else {
        logEvent(QStringLiteral("事件 %1").arg(type));
    }
}
