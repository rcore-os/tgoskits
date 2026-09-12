/* gui_interact.cpp - per-interaction testing: inject real events with QTest, assert STATE + re-rendered pixels.
 *
 * Every leg drives a real widget through a real event and predicts the outcome:
 *   - QPushButton + clicked handler -> QTest::mouseClick -> the handler counter incremented exactly once,
 *     and a second click makes it two (event delivery is real, not simulated by calling the slot).
 *   - QCheckBox -> mouseClick toggles isChecked(); a second click toggles back; the grabbed indicator
 *     pixels differ between checked and unchecked (visible state change, not just the bool).
 *   - QLineEdit -> QTest::keyClicks("hello") -> text()=="hello"; a backspace key -> "hell".
 *   - QSlider -> setValue baseline, then arrow-key press -> value() moved by exactly singleStep.
 *   - QPushButton disabled -> mouseClick does NOT fire the handler (negative control).
 *
 * QT_QPA_PLATFORM=offscreen: events are posted through the real Qt event loop against off-screen widgets.
 */
#include "gui_common.h"
#include <QApplication>
#include <QPushButton>
#include <QCheckBox>
#include <QLineEdit>
#include <QSlider>
#include <QTest>
#include <QImage>
#include <QPixmap>

/* ---- button click delivers a real event to a real handler ---- */
static void leg_button_click(Gate &g) {
    QPushButton btn("Go");
    btn.setFixedSize(80, 30);
    int fired = 0;
    QObject::connect(&btn, &QPushButton::clicked, [&fired]() { ++fired; });
    btn.show(); /* offscreen: no display, but the widget is realized for event routing */

    QTest::mouseClick(&btn, Qt::LeftButton, Qt::NoModifier, btn.rect().center());
    g.check(fired == 1, "button: first click did not fire handler exactly once");
    QTest::mouseClick(&btn, Qt::LeftButton, Qt::NoModifier, btn.rect().center());
    g.check(fired == 2, "button: second click did not increment to two");
}

/* ---- disabled button: click must NOT fire (negative control) ---- */
static void leg_button_disabled(Gate &g) {
    QPushButton btn("Off");
    btn.setFixedSize(80, 30);
    btn.setEnabled(false);
    int fired = 0;
    QObject::connect(&btn, &QPushButton::clicked, [&fired]() { ++fired; });
    btn.show();
    QTest::mouseClick(&btn, Qt::LeftButton, Qt::NoModifier, btn.rect().center());
    g.check(fired == 0, "button(disabled): click wrongly fired the handler");
}

/* ---- checkbox: state toggles AND the rendered indicator pixels change ---- */
static void leg_checkbox(Gate &g) {
    QCheckBox cb("opt");
    cb.setFixedSize(120, 24);
    cb.show();
    g.check(!cb.isChecked(), "checkbox: initial state not unchecked");

    QImage before = cb.grab().toImage().convertToFormat(QImage::Format_ARGB32);
    QTest::mouseClick(&cb, Qt::LeftButton, Qt::NoModifier, QPoint(8, cb.height() / 2));
    g.check(cb.isChecked(), "checkbox: click did not toggle to checked");
    QImage after = cb.grab().toImage().convertToFormat(QImage::Format_ARGB32);

    /* the indicator lives on the left; count differing pixels there between the two grabs */
    int diff = 0;
    int w = qMin(before.width(), after.width()), h = qMin(before.height(), after.height());
    int rx = qMin(24, w);
    for (int y = 0; y < h; ++y)
        for (int x = 0; x < rx; ++x)
            if (!argb_close(before.pixel(x, y), after.pixel(x, y), 8)) ++diff;
    g.check(diff > 4, "checkbox: indicator pixels did not change after toggle");

    /* second click toggles back to unchecked */
    QTest::mouseClick(&cb, Qt::LeftButton, Qt::NoModifier, QPoint(8, cb.height() / 2));
    g.check(!cb.isChecked(), "checkbox: second click did not toggle back");
}

/* ---- line edit: typed keys land in text(), backspace removes ---- */
static void leg_lineedit(Gate &g) {
    QLineEdit le;
    le.setFixedSize(160, 28);
    le.show();
    le.setFocus();
    QTest::keyClicks(&le, "hello");
    g.check(le.text() == "hello", "lineedit: keyClicks did not produce 'hello'");
    QTest::keyClick(&le, Qt::Key_Backspace);
    g.check(le.text() == "hell", "lineedit: backspace did not yield 'hell'");
    /* select-all + type replaces */
    QTest::keyClick(&le, Qt::Key_A, Qt::ControlModifier);
    QTest::keyClicks(&le, "world");
    g.check(le.text() == "world", "lineedit: ctrl-a + type did not replace with 'world'");
}

/* ---- slider: arrow key moves value by exactly singleStep; page keys by pageStep ---- */
static void leg_slider(Gate &g) {
    QSlider s(Qt::Horizontal);
    s.setRange(0, 100);
    s.setSingleStep(5);
    s.setPageStep(20);
    s.setValue(50);
    s.setFixedSize(200, 24);
    s.show();
    s.setFocus();
    g.check(s.value() == 50, "slider: initial value not 50");

    /* a horizontal slider increases value on Key_Right by singleStep */
    QTest::keyClick(&s, Qt::Key_Right);
    g.check(s.value() == 55, "slider: Key_Right did not add singleStep (55)");
    QTest::keyClick(&s, Qt::Key_Left);
    g.check(s.value() == 50, "slider: Key_Left did not subtract singleStep (50)");
    QTest::keyClick(&s, Qt::Key_PageUp);
    g.check(s.value() == 70, "slider: PageUp did not add pageStep (70)");
    QTest::keyClick(&s, Qt::Key_Home);
    g.check(s.value() == 0, "slider: Home did not go to minimum");
    QTest::keyClick(&s, Qt::Key_End);
    g.check(s.value() == 100, "slider: End did not go to maximum");
}

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    Gate g("GUI_INTERACT");
    leg_button_click(g);
    leg_button_disabled(g);
    leg_checkbox(g);
    leg_lineedit(g);
    leg_slider(g);
    return g.finish();
}
