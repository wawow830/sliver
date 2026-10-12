// A counter on the Touch Bar from a Qt 6 app, in plain D-Bus (QtDBus, no sliver libraries).
// docs/integrating-apps.md explains it step by step.
//
//     g++ -std=c++17 -fPIC examples/qt/counter.cpp -o counter $(pkg-config --cflags --libs Qt6DBus Qt6Core)
//     ./counter
//
// NOT compiled in the sliver repo (no Qt headers on the development machine): written against
// the QtDBus documentation. If it needs a fix, please send it.

#include <QCoreApplication>
#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusInterface>
#include <QDBusObjectPath>
#include <QDBusReply>
#include <QDBusServiceWatcher>
#include <QDBusVariant>
#include <QVariantMap>

static const QString BUS = QStringLiteral("io.github.wawow830.Sliver");
static const QString ROOT = QStringLiteral("/io/github/wawow830/Sliver");

class Counter : public QObject {
    Q_OBJECT
public:
    Counter() {
        auto bus = QDBusConnection::sessionBus();
        // aa{sv} and a{sa{sv}} are lists/maps of QVariantMap: tell QtDBus how to marshal them.
        qDBusRegisterMetaType<QList<QVariantMap>>();
        qDBusRegisterMetaType<QMap<QString, QVariantMap>>();
        // Publish when the agent appears (now, later or after a restart).
        auto *watcher = new QDBusServiceWatcher(BUS, bus,
            QDBusServiceWatcher::WatchForOwnerChange, this);
        connect(watcher, &QDBusServiceWatcher::serviceOwnerChanged, this,
                [this](const QString &, const QString &, const QString &now) {
                    path.clear();
                    if (!now.isEmpty()) publish();
                });
        if (bus.interface()->isServiceRegistered(BUS)) publish();
    }

private slots:
    // Event(s item, s kind, a{sv} data): sent to us only.
    void onEvent(const QString &item, const QString &kind, const QVariantMap &) {
        if (item != "count") return;
        if (kind == "tap") count++;
        else if (kind == "hold") count = 0;
        else return;
        QMap<QString, QVariantMap> changes;
        changes["count"] = {{"label", label()}, {"values", values()}};
        QDBusInterface ctx(BUS, path, BUS + ".Context1", QDBusConnection::sessionBus());
        ctx.asyncCall("Update", QVariant::fromValue(changes));
    }

private:
    int count = 0;
    QString path;

    QString label() const { return QStringLiteral("Clicked %1").arg(count); }
    QVariantMap values() const {
        // a{sv}: values the user can use in their own `format`
        return {{"count", count}};
    }

    void publish() {
        auto bus = QDBusConnection::sessionBus();
        QDBusInterface touchbar(BUS, ROOT, BUS + ".TouchBar1", bus);
        QVariantMap options{{"scope", "status"},
                            {"provider", "org.example.QtCounter"},
                            {"title", "Qt counter"}};
        QDBusReply<QDBusObjectPath> r = touchbar.call("CreateContext", options);
        if (!r.isValid()) return;
        path = r.value().path();
        QDBusInterface ctx(BUS, path, BUS + ".Context1", bus);
        QVariantMap item{{"id", "count"}, {"type", "button"}, {"label", label()},
                         {"gestures", QStringList{"hold"}}, {"values", values()}};
        ctx.call("SetItems", QVariant::fromValue(QList<QVariantMap>{item}));
        bus.connect(BUS, path, BUS + ".Context1", "Event", this,
                    SLOT(onEvent(QString, QString, QVariantMap)));
    }
};

int main(int argc, char **argv) {
    QCoreApplication app(argc, argv);
    Counter counter;
    return app.exec();
}

#include "counter.moc"
