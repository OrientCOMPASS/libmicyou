/* libmicyou — Qt 6 reference frontend. */
#include "session.h"

#include <QCoreApplication>
#include <QDir>
#include <QJsonArray>
#include <QJsonDocument>
#include <QFileInfo>
#include <QStandardPaths>

Session::Session(QObject *parent) : QObject(parent) {}

Session::~Session()
{
    if (m_proc) {
        m_proc->closeWriteChannel(); // EOF → daemon exits gracefully
        if (!m_proc->waitForFinished(1500))
            m_proc->kill();
    }
}

bool Session::isConnected() const
{
    return m_proc && m_proc->state() != QProcess::NotRunning;
}

bool Session::start(QString *errorOut)
{
    if (isConnected())
        return true;

    QString program;
    QStringList candidates;
#ifdef Q_OS_WIN
    const QString exe = QStringLiteral("micyou-daemon.exe");
#else
    const QString exe = QStringLiteral("micyou-daemon");
#endif
    const QString fromEnv = qEnvironmentVariable("MICYOU_DAEMON");
    if (!fromEnv.isEmpty())
        candidates << fromEnv;
    candidates << QCoreApplication::applicationDirPath() + QLatin1Char('/') + exe;
    const QString onPath = QStandardPaths::findExecutable(exe);
    if (!onPath.isEmpty())
        candidates << onPath;

    for (const QString &c : candidates) {
        if (QFileInfo::exists(c)) {
            program = c;
            break;
        }
    }
    if (program.isEmpty()) {
        if (errorOut)
            *errorOut = QStringLiteral(
                "micyou-daemon not found (looked in $MICYOU_DAEMON, app dir, PATH)");
        return false;
    }

    m_proc = new QProcess(this);
    m_proc->setProcessChannelMode(QProcess::SeparateChannels);
    connect(m_proc, &QProcess::readyReadStandardOutput, this, &Session::onReadyRead);
    connect(m_proc, &QProcess::errorOccurred, this, &Session::onProcessError);
    connect(m_proc, &QProcess::finished, this, [this](int code, QProcess::ExitStatus) {
        failAll(QStringLiteral("daemon exited (%1)").arg(code));
        emit disconnected(code);
    });
    m_proc->start(program, {QStringLiteral("--stdio"), QStringLiteral("--no-mode-lock")});
    if (!m_proc->waitForStarted(5000)) {
        if (errorOut)
            *errorOut = QStringLiteral("failed to spawn %1: %2").arg(program, m_proc->errorString());
        m_proc->deleteLater();
        m_proc = nullptr;
        return false;
    }

    // hello + subscribe (fire-and-forget responses surface via callback logs)
    call(QStringLiteral("session/hello"),
         QJsonObject{{"name", "qt-frontend"}, {"ui", true}},
         [](const QJsonValue &, const QString &) {});
    call(QStringLiteral("session/subscribe"),
         QJsonObject{{"events", QJsonArray{QStringLiteral("*")}}},
         [](const QJsonValue &, const QString &) {});
    return true;
}

void Session::call(const QString &method, const QJsonObject &params, Callback cb)
{
    if (!isConnected()) {
        if (cb)
            cb(QJsonValue(), QStringLiteral("daemon not connected"));
        return;
    }
    const qint64 id = m_nextId++;
    QJsonObject req{
        {"jsonrpc", "2.0"},
        {"id", id},
        {"method", method},
        {"params", params},
    };
    if (cb)
        m_pending.insert(id, std::move(cb));
    QByteArray line = QJsonDocument(req).toJson(QJsonDocument::Compact);
    line.append('\n');
    m_proc->write(line);
}

void Session::onReadyRead()
{
    m_buffer.append(m_proc->readAllStandardOutput());
    int nl;
    while ((nl = m_buffer.indexOf('\n')) >= 0) {
        const QByteArray line = m_buffer.left(nl).trimmed();
        m_buffer.remove(0, nl + 1);
        if (!line.isEmpty())
            handleLine(line);
    }
}

void Session::handleLine(const QByteArray &line)
{
    QJsonParseError perr;
    const QJsonDocument doc = QJsonDocument::fromJson(line, &perr);
    if (perr.error != QJsonParseError::NoError || !doc.isObject())
        return;
    const QJsonObject msg = doc.object();

    // Notification (no id) → event fan-out
    if (!msg.contains(QStringLiteral("id"))) {
        if (msg.value(QStringLiteral("method")).toString() == QLatin1String("event")) {
            const QJsonObject params = msg.value(QStringLiteral("params")).toObject();
            emit eventReceived(params.value(QStringLiteral("type")).toString(),
                               params.value(QStringLiteral("data")).toObject());
        }
        return;
    }

    const qint64 id = static_cast<qint64>(msg.value(QStringLiteral("id")).toDouble(-1));
    auto it = m_pending.find(id);
    if (it == m_pending.end())
        return;
    Callback cb = *it;
    m_pending.erase(it);
    if (msg.contains(QStringLiteral("error"))) {
        const QJsonObject err = msg.value(QStringLiteral("error")).toObject();
        cb(QJsonValue(), err.value(QStringLiteral("message")).toString());
    } else {
        cb(msg.value(QStringLiteral("result")), QString());
    }
}

void Session::onProcessError(QProcess::ProcessError error)
{
    if (error == QProcess::FailedToStart)
        failAll(QStringLiteral("daemon failed to start"));
}

void Session::failAll(const QString &reason)
{
    const auto pending = m_pending;
    m_pending.clear();
    for (auto it = pending.begin(); it != pending.end(); ++it)
        it.value()(QJsonValue(), reason);
}
