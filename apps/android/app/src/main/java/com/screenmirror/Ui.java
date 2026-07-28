package com.screenmirror;

import android.content.Context;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.LinearLayout;
import android.widget.SeekBar;
import android.widget.TextView;

/**
 * Shared look and feel for the programmatic UI: a dark surface palette, rounded cards, and
 * accent-tinted controls. Keeping it here stops {@link MainActivity} from turning into styling code.
 */
final class Ui {
    static final int BACKGROUND = 0xFF0E1116;
    static final int SURFACE = 0xFF171C24;
    static final int FIELD = 0xFF212936;
    static final int OUTLINE = 0xFF2C3542;
    static final int ACCENT = 0xFF40BFFF;
    static final int ON_ACCENT = 0xFF06121C;
    static final int TEXT = 0xFFE8EEF5;
    static final int TEXT_MUTED = 0xFF8B97A7;
    static final int DANGER = 0xFFFF6B6B;

    private Ui() {
    }

    static int dp(Context context, float value) {
        return Math.round(TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP,
                value,
                context.getResources().getDisplayMetrics()
        ));
    }

    static LinearLayout card(Context context) {
        LinearLayout card = new LinearLayout(context);
        card.setOrientation(LinearLayout.VERTICAL);
        GradientDrawable background = new GradientDrawable();
        background.setColor(SURFACE);
        background.setCornerRadius(dp(context, 18));
        background.setStroke(dp(context, 1), OUTLINE);
        card.setBackground(background);
        int padding = dp(context, 16);
        card.setPadding(padding, padding, padding, padding);
        return card;
    }

    static LinearLayout.LayoutParams cardParams(Context context) {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
        );
        params.setMargins(dp(context, 16), dp(context, 8), dp(context, 16), dp(context, 8));
        return params;
    }

    static LinearLayout row(Context context) {
        LinearLayout row = new LinearLayout(context);
        row.setOrientation(LinearLayout.HORIZONTAL);
        return row;
    }

    static LinearLayout.LayoutParams weighted(Context context) {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                0,
                LinearLayout.LayoutParams.WRAP_CONTENT,
                1.0f
        );
        params.setMargins(dp(context, 4), 0, dp(context, 4), 0);
        return params;
    }

    static TextView sectionTitle(Context context, String text) {
        TextView view = new TextView(context);
        view.setText(text);
        view.setTextColor(TEXT);
        view.setTypeface(Typeface.DEFAULT_BOLD);
        view.setTextSize(TypedValue.COMPLEX_UNIT_SP, 17);
        return view;
    }

    static TextView caption(Context context, String text) {
        TextView view = new TextView(context);
        view.setText(text);
        view.setTextColor(TEXT_MUTED);
        view.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f);
        view.setPadding(0, dp(context, 2), 0, dp(context, 10));
        return view;
    }

    static TextView label(Context context, String text) {
        TextView view = new TextView(context);
        view.setText(text);
        view.setTextColor(TEXT_MUTED);
        view.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        return view;
    }

    static Button primaryButton(Context context, String text, View.OnClickListener listener) {
        return styledButton(context, text, listener, ACCENT, ON_ACCENT, 0);
    }

    static Button secondaryButton(Context context, String text, View.OnClickListener listener) {
        return styledButton(context, text, listener, 0x00000000, ACCENT, ACCENT);
    }

    static Button quietButton(Context context, String text, View.OnClickListener listener) {
        return styledButton(context, text, listener, FIELD, TEXT, OUTLINE);
    }

    static Button dangerButton(Context context, String text, View.OnClickListener listener) {
        return styledButton(context, text, listener, 0x00000000, DANGER, DANGER);
    }

    private static Button styledButton(
            Context context,
            String text,
            View.OnClickListener listener,
            int fill,
            int textColor,
            int strokeColor
    ) {
        Button button = new Button(context);
        button.setText(text);
        button.setAllCaps(false);
        button.setTextColor(textColor);
        button.setTypeface(Typeface.DEFAULT_BOLD);
        button.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14.5f);
        button.setStateListAnimator(null);
        button.setMinHeight(dp(context, 48));
        button.setPadding(dp(context, 12), 0, dp(context, 12), 0);
        GradientDrawable background = new GradientDrawable();
        background.setColor(fill);
        background.setCornerRadius(dp(context, 14));
        if (strokeColor != 0) {
            background.setStroke(dp(context, 1), strokeColor);
        }
        button.setBackground(background);
        button.setOnClickListener(listener);
        return button;
    }

    static GradientDrawable fieldBackground(Context context) {
        GradientDrawable background = new GradientDrawable();
        background.setColor(FIELD);
        background.setCornerRadius(dp(context, 14));
        background.setStroke(dp(context, 1), OUTLINE);
        return background;
    }

    static GradientDrawable pillBackground(Context context, int color) {
        GradientDrawable background = new GradientDrawable();
        background.setColor(0x00000000);
        background.setCornerRadius(dp(context, 999));
        background.setStroke(dp(context, 1), color);
        return background;
    }

    static void tint(CheckBox checkBox) {
        checkBox.setTextColor(TEXT);
        checkBox.setButtonTintList(ColorStateList.valueOf(ACCENT));
    }

    static void tint(SeekBar seekBar) {
        ColorStateList accent = ColorStateList.valueOf(ACCENT);
        seekBar.setProgressTintList(accent);
        seekBar.setThumbTintList(accent);
        seekBar.setProgressBackgroundTintList(ColorStateList.valueOf(OUTLINE));
    }

    static TextView statusPill(Context context) {
        TextView view = new TextView(context);
        view.setTextColor(TEXT);
        view.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        view.setGravity(Gravity.CENTER_VERTICAL);
        view.setBackground(pillBackground(context, OUTLINE));
        view.setPadding(dp(context, 14), dp(context, 8), dp(context, 14), dp(context, 8));
        return view;
    }
}
