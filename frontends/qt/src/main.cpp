/* libmicyou — Qt 6 reference frontend. */
#include "mainwindow.h"

#include <QApplication>

int main(int argc, char *argv[])
{
    QApplication app(argc, argv);
    QApplication::setApplicationName(QStringLiteral("micyou-qt-frontend"));
    QApplication::setOrganizationName(QStringLiteral("libmicyou"));
    QApplication::setApplicationVersion(QStringLiteral("0.1.0"));

    MainWindow window;
    window.show();
    return QApplication::exec();
}
