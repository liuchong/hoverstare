const copyButtons = document.querySelectorAll("[data-copy]");

copyButtons.forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.querySelector(button.dataset.copy);
    if (!target) {
      return;
    }

    try {
      await navigator.clipboard.writeText(target.textContent.trim());
      const previous = button.textContent;
      button.textContent = "Copied";
      window.setTimeout(() => {
        button.textContent = previous;
      }, 1600);
    } catch {
      button.textContent = "Select";
      target.parentElement?.focus();
    }
  });
});
