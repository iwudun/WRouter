import * as React from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";

export type SelectOption = {
  disabled?: boolean;
  label: React.ReactNode;
  value: string;
};

export interface SelectProps extends React.SelectHTMLAttributes<HTMLSelectElement> {
  onValueChange?: (value: string) => void;
  options?: SelectOption[];
}

// Native <select> never exposes whether its dropdown popup is open. Relying on
// :focus alone is misleading: clicking the select again to *close* the popup
// keeps focus on it, so a focus-driven caret gets stuck pointing up. We track an
// internal `open` flag instead — toggled on pointerdown, reset on blur/change —
// which mirrors the real state for the common mouse interactions.
function SelectBody({
  selectRef,
  children,
  className,
  onChange,
  onValueChange,
  options,
  onBlur,
  onPointerDown,
  ...props
}: SelectProps & { selectRef: React.Ref<HTMLSelectElement> }) {
  const [open, setOpen] = React.useState(false);
  return (
    <div className="relative">
      <select
        // ``color-scheme: light dark`` lets the browser pick the native form-control
        // palette that matches the current :root[data-theme] without us having to
        // know which mode is active. Without it, the embedded Chromium renders the
        // closed select with a light background even in dark mode.
        // ``option:bg-background option:text-foreground`` are not honoured by
        // Chromium for the *open* dropdown popup (it draws the popup outside the
        // page DOM), so the only effective lever is ``color-scheme`` combined with
        // these explicit class hooks — which fix the collapsed control's colours
        // and let users on platforms that *do* honour option styling benefit too.
        className={cn(
          "peer h-8 w-full min-w-0 appearance-none rounded-md border border-input bg-background px-3 pr-8 text-body text-foreground shadow-[inset_0_1px_1px_rgba(0,0,0,0.03)] outline-none transition-[background-color,border-color,box-shadow,color] hover:border-muted-foreground/45 focus:border-primary/60 focus:ring-2 focus:ring-ring/25 disabled:cursor-not-allowed disabled:opacity-50",
          className
        )}
        // ``colorScheme`` is a React/DOM camelCase that maps to the ``color-scheme``
        // CSS property. Setting it here means every <select> follows whatever the
        // root <html data-theme> is — the dropdown popup therefore renders with
        // the right OS-native palette (dark text on light background or light
        // text on dark background) instead of being stuck in the agent default
        // which is what produced the "all white" complaint.
        style={{ colorScheme: "light dark" }}
        onBlur={(event) => {
          onBlur?.(event);
          setOpen(false);
        }}
        onChange={(event) => {
          onChange?.(event);
          onValueChange?.(event.target.value);
          setOpen(false);
        }}
        onPointerDown={(event) => {
          onPointerDown?.(event);
          setOpen((prev) => !prev);
        }}
        ref={selectRef}
        {...props}
      >
        {options
          ? options.map((option) => (
              <option
                className="bg-background text-foreground"
                disabled={option.disabled}
                key={option.value}
                value={option.value}
              >
                {option.label}
              </option>
            ))
          : children}
      </select>
      <ChevronDown
        aria-hidden
        className={cn(
          "pointer-events-none absolute right-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground transition-[color,rotate] duration-150 peer-disabled:opacity-50 peer-hover:text-foreground peer-focus:text-foreground",
          open && "rotate-180"
        )}
      />
    </div>
  );
}

const Select = React.forwardRef<HTMLSelectElement, SelectProps>((props, ref) => (
  <SelectBody {...props} selectRef={ref} />
));

Select.displayName = "Select";

export { Select };
