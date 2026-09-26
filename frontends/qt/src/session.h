/*
 * libmicyou — Qt 6 reference frontend.
 * Copyright (C) 2026 OrientCOMPASS
 * GPL-3.0-or-later with the MicYou Plugin Exception.
 */
#pragma once

#include <QByteArray>
#include <QHash>
#include <QJsonObject>
#include <QJsonValue>
#include <QObject>
#include <QProcess>
#include <QString>
#include <functional>

/// JSON-RPC 2.0 session with the micyou-daemon sidecar over stdio.
/// Idiomatic Qt: async calls with callbacks + signals for backend events.
class Session : public QObject {
    Q_OBJECT
public:
    using Callback = std::function<void(const QJsonValue &result, const QString &error)>;

    explicit Session(QObject *parent = nullptr);
    ~Session() override;

    /// Locate and spawn the daemon (env MICYOU_DAEMON → sibling of this
    /// executable → PATH), then perform hello + subscribe("*").
    bool start(QString *errorOut = nullptr);
    bool isConnected() const;

    /// Fire an RPC call; the callback runs on the UI thread when the
    /// response (or a transport error) arrives.
    void call(const QString &method, const QJsonObject &params, Callback cb);

signals:
    /// A backend event notification: {"type":..., "data":{...}} params.
    void eventReceived(const QString &type, const QJsonObject &data);
    /// The daemon process ended.
    void disconnected(int exitCode);

private slots:
    void onReadyRead();
    void onProcessError(QProcess::ProcessError error);

private:
    void handleLine(const QByteArray &line);
    void failAll(const QString &reason);

    QProcess *m_proc = nullptr;
    QByteArray m_buffer;
    qint64 m_nextId = 1;
    QHash<qint64, Callback> m_pending;
};
