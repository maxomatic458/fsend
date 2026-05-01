import type { JSX } from "solid-js";

export type ButtonVariant = "blue" | "green" | "red" | "orange" | "gray";

const variantStyles: Record<ButtonVariant, string> = {
  blue: "border-blue-700 dark:border-blue-600 bg-blue-100 dark:bg-blue-900/60 text-blue-900 dark:text-blue-100 hover:bg-blue-200 dark:hover:bg-blue-800/60",
  green:
    "border-green-700 dark:border-green-600 bg-green-100 dark:bg-green-900/60 text-green-900 dark:text-green-100 hover:bg-green-200 dark:hover:bg-green-800/60",
  red: "border-red-700 dark:border-red-600 bg-red-100 dark:bg-red-900/60 text-red-900 dark:text-red-100 hover:bg-red-200 dark:hover:bg-red-800/60",
  orange:
    "border-orange-700 dark:border-orange-600 bg-orange-100 dark:bg-orange-900/60 text-orange-900 dark:text-orange-100 hover:bg-orange-200 dark:hover:bg-orange-800/60",
  gray: "border-gray-500 dark:border-gray-600 bg-gray-100 dark:bg-gray-700/60 text-gray-700 dark:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-600/60",
};

interface ButtonProps {
  variant: ButtonVariant;
  onClick?: () => void;
  disabled?: boolean;
  class?: string;
  children: JSX.Element;
}

export function Button(props: ButtonProps) {
  return (
    <button
      onClick={props.onClick}
      disabled={props.disabled}
      class={`py-2 px-4 rounded-xl font-semibold transition-all duration-150 border-2 disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer ${variantStyles[props.variant]} ${props.class ?? ""}`}
    >
      {props.children}
    </button>
  );
}
