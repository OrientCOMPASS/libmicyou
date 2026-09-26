/*
 * libmicyou — Qt 6 reference frontend.
 * Copyright (C) 2026 OrientCOMPASS
 * GPL-3.0-or-later with the MicYou Plugin Exception.
 */
#pragma once

#include <QJsonArray>
#include <QJsonObject>
#include <QList>
#include <QMainWindow>
#include <QStringList>

class QCheckBox;
class QComboBox;
class QLabel;
class QLineEdit;
class QListWidget;
class QPlainTextEdit;
class QProgressBar;
class QSlider;
class QSpinBox;
class QDoubleSpinBox;
class QStackedWidget;
class QTableWidget;
class Session;

/// Classic Qt desktop shell: menu bar + toolbar + list/stack navigation +
/// dockable event log + status bar telemetry. Deliberately native-looking
/// (system widget style) — the Qt identity of this frontend family.
class MainWindow : public QMainWindow {
    Q_OBJECT
public:
    explicit MainWindow(QWidget *parent = nullptr);

private slots:
    void onEvent(const QString &type, const QJsonObject &data);
    void navigate(int row);

private:
    // page builders
    QWidget *buildOverviewPage();
    QWidget *buildAudioPage();
    QWidget *buildConnectionPage();
    QWidget *buildDevicesPage();
    QWidget *buildPluginsPage();
    QWidget *buildSettingsPage();
    QWidget *buildSystemPage();

    // actions
    void doStart();
    void doStop();
    void setMuted(bool muted);
    void setMonitoring(bool enabled);
    void refreshStatus();
    void loadAudioPage();
    void saveDsp();
    void loadConnectionPage();
    void loadDevicesPage();
    void loadPlugins();
    void selectPlugin(int row);
    void loadSettingsPage();
    void savePrefs();
    void loadSystemPage();

    void logEvent(const QString &line);
    void setStatusPills(const QString &phase, bool running, bool connected, bool muted);

    Session *m_session = nullptr;

    // chrome
    QListWidget *m_nav = nullptr;
    QStackedWidget *m_pages = nullptr;
    QLabel *m_phaseLabel = nullptr;
    QLabel *m_deviceLabel = nullptr;
    QProgressBar *m_levelBar = nullptr;
    QPlainTextEdit *m_eventLog = nullptr;
    QAction *m_actMute = nullptr;
    QAction *m_actMonitor = nullptr;

    // overview
    QLineEdit *m_qPort = nullptr;
    QComboBox *m_qMode = nullptr;
    QLabel *m_ovStatus = nullptr;

    // audio/dsp
    QComboBox *m_outDevice = nullptr;
    QSpinBox *m_bufferMs = nullptr;
    QCheckBox *m_chkMute = nullptr;
    QCheckBox *m_chkMonitor = nullptr;
    QCheckBox *m_chkAec = nullptr;
    QLabel *m_aecStatus = nullptr;
    QSlider *m_gain = nullptr;
    QLabel *m_gainLabel = nullptr;
    QCheckBox *m_nsEn = nullptr;
    QComboBox *m_nsType = nullptr;
    QSlider *m_nsIntensity = nullptr;
    QCheckBox *m_drvEn = nullptr;
    QSlider *m_drvLevel = nullptr;
    QCheckBox *m_agcEn = nullptr;
    QSpinBox *m_agcTarget = nullptr;
    QSlider *m_agcAttack = nullptr;
    QSlider *m_agcDecay = nullptr;
    QCheckBox *m_vadEn = nullptr;
    QSlider *m_vadThreshold = nullptr;
    QCheckBox *m_eqEn = nullptr;
    QSlider *m_eqPreamp = nullptr;
    QList<QSlider *> m_eqBands;
    QLabel *m_chainLabel = nullptr;
    QJsonObject m_dspCache;
    QStringList m_deviceCache;

    // connection
    QLabel *m_netIps = nullptr;
    QTableWidget *m_ifaceTable = nullptr;
    QComboBox *m_usbDevices = nullptr;
    QLabel *m_usbResult = nullptr;
    QLabel *m_webStatus = nullptr;
    QLabel *m_webUrls = nullptr;
    QStringList m_usbSerials;

    // devices
    QLabel *m_vbcStatus = nullptr;
    QPlainTextEdit *m_vbcProgress = nullptr;
    QLabel *m_bhStatus = nullptr;
    QLabel *m_pwStatus = nullptr;

    // plugins
    QListWidget *m_pluginList = nullptr;
    QLabel *m_pdMeta = nullptr;
    QLabel *m_pdCaps = nullptr;
    QCheckBox *m_pdEnabled = nullptr;
    QPlainTextEdit *m_pdConfig = nullptr;
    QLineEdit *m_pdAction = nullptr;
    QLineEdit *m_pdPayload = nullptr;
    QPlainTextEdit *m_pdLog = nullptr;
    QJsonArray m_pluginCache;
    int m_selectedPlugin = -1;

    // settings
    QSpinBox *m_spPort = nullptr;
    QSpinBox *m_spWebPort = nullptr;
    QComboBox *m_spMode = nullptr;
    QLineEdit *m_spBind = nullptr;
    QCheckBox *m_spAutoBind = nullptr;
    QLineEdit *m_spDevice = nullptr;
    QCheckBox *m_spMuteSync = nullptr;
    QLabel *m_prefsExists = nullptr;
    QLineEdit *m_uiLang = nullptr;
    QLineEdit *m_uiColor = nullptr;
    QLabel *m_themeSummary = nullptr;

    // system
    QLabel *m_sysVersion = nullptr;
    QLabel *m_sysMode = nullptr;
    QLabel *m_sysUpdate = nullptr;
    QLabel *m_logPath = nullptr;
    QPlainTextEdit *m_logContent = nullptr;

    QString m_os;
};
