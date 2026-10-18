// @vitest-environment happy-dom
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import Button from "./Button.vue";

describe("Button", () => {
  it("paints the variant and the size it was asked for", () => {
    // The variants are the only vocabulary the app has for how loud an action is, so the
    // prop has to land on the class the stylesheet draws, not merely be accepted.
    const classes = mount(Button, { props: { variant: "tinted", size: "sm" } }).classes();
    expect(classes).toContain("muster-button");
    expect(classes).toContain("muster-button-tinted");
    expect(classes).toContain("muster-button-sm");
  });

  it("defaults to subtle and exposes the filled variant without using it as the default", () => {
    const classes = mount(Button).classes();
    expect(classes).toContain("muster-button-subtle");
    expect(classes).toContain("muster-button-md");

    const filledClasses = mount(Button, { props: { variant: "filled", size: "lg" } }).classes();
    expect(filledClasses).toContain("muster-button-filled");
    expect(filledClasses).toContain("muster-button-lg");
  });

  it("maps every available variant to its shared class", () => {
    for (const variant of ["filled", "tinted", "outlined", "subtle", "ghost", "danger"] as const) {
      expect(mount(Button, { props: { variant } }).classes()).toContain(`muster-button-${variant}`);
    }
  });

  it("submits only when it is the submit button, and never answers while disabled", async () => {
    const wrapper = mount(Button, { props: { type: "submit", disabled: true } });
    expect(wrapper.attributes("type")).toBe("submit");
    expect(wrapper.attributes("disabled")).toBeDefined();
    await wrapper.trigger("click");
    expect(wrapper.emitted("click")).toBeUndefined();
  });
});
