# Recovering Deferred Images

Documentation pages often defer large diagrams until they approach the viewport. A text reader still needs the real resource URL, alternative text, and caption even though it never executes the page's JavaScript.

![The document processing pipeline](https://example.test/guides/read/images/pipeline-large.webp) The document processing pipeline.

Some loaders keep the destination in a data attribute instead of a source set. The extracted document should publish that destination as an ordinary image without retaining loader-specific attributes.

![A map loaded from a data attribute](https://example.test/guides/media/deferred-map.png) A map loaded from a data attribute.

Responsive galleries often wrap image-only cards in layout containers. Those cards remain meaningful even when they add little visible text to the article scoring model.

![First gallery diagram](https://example.test/gallery/first.jpg) First gallery diagram.

![Second gallery diagram](https://example.test/gallery/second.jpg) Second gallery diagram.

Server-rendered applications may place the only usable image inside a noscript fallback beside a transparent placeholder. Static extraction must recover the fallback before removing the noscript wrapper.

![A server-rendered fallback diagram](https://example.test/assets/static-fallback.svg) A server-rendered fallback diagram.
