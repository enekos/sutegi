<script lang="ts">
  import Landing from './Landing.svelte'
  import Docs from './Docs.svelte'

  // Hash routing that does not collide with the landing page's in-page
  // #anchor links: a route is only a hash that begins with "#/".
  //   #features        -> landing (browser scrolls to the section)
  //   #/docs           -> docs page, top
  //   #/docs/routing   -> docs page, scrolled to the "routing" section
  function parse() {
    const h = window.location.hash
    if (h.startsWith('#/')) {
      const [page, section] = h.slice(2).split('/')
      return { page: page || 'home', section: section || '' }
    }
    return { page: 'home', section: '' }
  }

  let route = $state(parse())
  $effect(() => {
    const on = () => (route = parse())
    window.addEventListener('hashchange', on)
    return () => window.removeEventListener('hashchange', on)
  })
</script>

{#if route.page === 'docs'}
  <Docs section={route.section} />
{:else}
  <Landing />
{/if}
